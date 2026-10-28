// The on-device agent of testbox/tools `android`. Android gives every app's windows and their
// accessibility events to one UiAutomation connection at a time (`uiautomator dump` beside
// `uiautomator events` is killed), so the waits live here: a look now and after each event, never
// on a timer, then the screen let settle, as uiautomator's dump does.
// One JSON line each way on an abstract Unix socket. The protocol's types are generated from
// testbox/tools/src/agent.rs (package `protocol`).
package org.b33p.testbox.agent

import android.accessibilityservice.AccessibilityServiceInfo
import android.app.Instrumentation
import android.app.UiAutomation
import android.graphics.Rect
import android.graphics.Region
import android.net.LocalServerSocket
import android.net.LocalSocket
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.util.Log
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityNodeInfo.AccessibilityAction
import java.io.IOException
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.thread
import kotlin.concurrent.withLock
import kotlinx.serialization.json.Json
import org.b33p.testbox.agent.protocol.Call
import org.b33p.testbox.agent.protocol.Echo
import org.b33p.testbox.agent.protocol.Found
import org.b33p.testbox.agent.protocol.Node
import org.b33p.testbox.agent.protocol.Outcome
import org.b33p.testbox.agent.protocol.Reply
import org.b33p.testbox.agent.protocol.Request
import org.b33p.testbox.agent.protocol.Wanted

/** The abstract socket's name, which `adb forward … localabstract:` reaches. */
private const val SOCKET = "b33p-testbox-agent"

/** How long without events counts as settled; uiautomator's dump waits as long. */
private const val QUIET_MS = 500L

/** Settling gives up after this, as uiautomator's dump does: a camera preview never settles. */
private const val SETTLE_MS = 10_000L

/** Events closer than this come as one (AccessibilityServiceInfo `notificationTimeout`): a look
 * per batch, not per event, while an animation runs. */
private const val EVENT_BATCH_MS = 100L

private const val TAG = "testbox-agent"

/** The users adbd runs as (android.os.Process `SHELL_UID`, `ROOT_UID`, not public SDK). */
private const val SHELL_UID = 2000
private const val ROOT_UID = 0

/** A press this long opens a row's menu (Android's long press is 400 ms by default,
 * ViewConfiguration `DEFAULT_LONG_PRESS_TIMEOUT`). */
private const val LONG_PRESS_MS = 1000

/** How long a swipe may bring no change before the screen is looked at again. */
private const val SWIPE_STILL_MS = 2_000L

/** What a list offers while it can scroll on: forward, or down as Compose's lists also say. */
private val SCROLLS = listOf(AccessibilityAction.ACTION_SCROLL_FORWARD, AccessibilityAction.ACTION_SCROLL_DOWN)

/** The protocol's sum types are tagged by `kind` (testbox/tools/src/agent.rs). */
private val json = Json { classDiscriminator = "kind" }

class Agent : Instrumentation() {
  override fun onCreate(arguments: Bundle?) {
    super.onCreate(arguments)
    start()
  }

  override fun onStart() {
    val automation = getUiAutomation(UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES)
    automation.serviceInfo =
      automation.serviceInfo.apply {
        // Popups and dialogs are windows of their own; Compose's text nodes are "not important".
        flags =
          flags or
            AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS or
            AccessibilityServiceInfo.FLAG_INCLUDE_NOT_IMPORTANT_VIEWS
        notificationTimeout = EVENT_BATCH_MS
      }
    val device = Device(automation)
    val server = LocalServerSocket(SOCKET)
    // What the driver waits for: `am instrument -r` prints it.
    sendStatus(1, Bundle().apply { putString("listening", SOCKET) })
    while (true) {
      val connection = server.accept()
      // adb's forward only (adbd runs as the shell user, or root): not an app on the device, as
      // an abstract socket is anyone's to connect to.
      val uid = connection.peerCredentials.uid
      if (uid != SHELL_UID && uid != ROOT_UID) {
        Log.w(TAG, "refused a connection from uid $uid")
        connection.close()
        continue
      }
      // The newest connection is the driver's: an older one's wait ends now, and it can't tap, swipe or
      // type beside it.
      val screen = device.takeOver()
      thread(name = "connection") {
        try {
          connection.use { Session(screen).serve(it) }
        } catch (error: IOException) {
          Log.w(TAG, "a connection failed", error)
        }
      }
    }
  }
}

/** One connection's requests, and the nodes of its last answered screen, which a Click's handle
 * names. */
private class Session(private val screen: Screen) {
  private var last = emptyList<AccessibilityNodeInfo>()

  fun serve(connection: LocalSocket) {
    val reader = connection.inputStream.bufferedReader()
    val writer = connection.outputStream.bufferedWriter()
    while (true) {
      val line = reader.readLine() ?: return
      writer.write(json.encodeToString(Reply.serializer(), answer(line)))
      writer.newLine()
      writer.flush()
    }
  }

  private fun answer(line: String): Reply {
    val request =
      try {
        json.decodeFromString(Request.serializer(), line)
      } catch (error: IllegalArgumentException) {
        return Reply(0u, Outcome.Failed("not a request: ${error.message}: $line"))
      }
    return try {
      val looked: Look? =
        when (val call = request.call) {
          is Call.WaitFor -> screen.waitFor(call.params.wanted, call.params.timeout_ms.toLong())
          is Call.WaitGone -> {
            screen.waitGone(call.params.wanted, call.params.timeout_ms.toLong())
            null
          }
          is Call.SetText -> {
            screen.setText(call.params.text, call.params.echo, call.params.timeout_ms.toLong())
            null
          }
          is Call.ScrollTo -> screen.scrollTo(call.params.label, call.params.timeout_ms.toLong())
          is Call.Click -> {
            screen.tap(last, call.params.handle.toInt(), call.params.long)
            null
          }
        }
      looked?.let { last = it.infos }
      Reply(request.id, looked?.let { Outcome.Done(it.found) } ?: Outcome.Acted)
    } catch (error: Exception) {
      Reply(request.id, Outcome.Failed(error.message ?: error.toString()))
    }
  }
}

/** Its text or content description is `label`, padding aside ("Note to Self  " by a badge). */
fun Node.labelled(label: String) = text.trim() == label || content_desc.trim() == label

fun Wanted.matches(node: Node): Boolean =
  when (this) {
    is Wanted.Label -> node.labelled(value)
    is Wanted.Containing -> node.text.contains(value)
    is Wanted.Control -> node.labelled(value) && node.enabled
    is Wanted.Editable -> node.editable
  }

/** Whether `field` reads as the echo says; its hint, while it shows one, is no text. */
fun Echo.matches(field: AccessibilityNodeInfo): Boolean {
  val text = if (field.isShowingHintText) "" else field.text?.toString().orEmpty()
  return when (this) {
    is Echo.Anything -> true
    is Echo.Exact -> text == value
    is Echo.Digits -> text.filter(Char::isDigit) == value
  }
}

/** What a look while scrolling found: the label, or a list to swipe on. */
private sealed interface ScrollStep {
  class Shown(val look: Look) : ScrollStep

  class Swipe(val list: Rect) : ScrollStep
}

/** A screen as answered: what the driver reads, and the nodes behind it, by handle. */
private class Look(val found: Found, val infos: List<AccessibilityNodeInfo>)

/** The device's one UiAutomation, its events counted, and which connection may use it. */
private class Device(val automation: UiAutomation) {
  private val lock = ReentrantLock()
  private val changed = lock.newCondition()

  /** Held while a connection acts on the screen, and while one takes over: taken before [lock]. */
  private val input = ReentrantLock()

  /** Accessibility events heard so far; under [lock]. */
  private var events = 0L

  /** The newest connection's; under [lock]. */
  private var owner = 0L

  init {
    automation.setOnAccessibilityEventListener {
      lock.withLock {
        events += 1
        changed.signalAll()
      }
    }
  }

  /** A screen for a new connection; the waits of the one before end, after its input in progress.
   */
  fun takeOver(): Screen =
    input.withLock {
      lock.withLock {
        owner += 1
        changed.signalAll()
        Screen(this, owner)
      }
    }

  fun events(): Long = lock.withLock { events }

  /** `act`, unless a newer connection than `owner` has taken over: then it throws. */
  fun <T> asOwner(owner: Long, act: () -> T): T =
    input.withLock {
      lock.withLock { checkOwnerLocked(owner) }
      act()
    }

  private fun checkOwnerLocked(owner: Long) = check(this.owner == owner) { "a newer connection took over" }

  /** The count of events once it is past `seen`; `null` if the deadline comes first. Throws once
   * a newer connection than `owner` has taken over. */
  fun nextChange(owner: Long, seen: Long, deadline: Long): Long? =
    lock.withLock {
      checkOwnerLocked(owner)
      while (events == seen) {
        val left = deadline - SystemClock.uptimeMillis()
        if (left <= 0) {
          return null
        }
        changed.await(left, TimeUnit.MILLISECONDS)
        checkOwnerLocked(owner)
      }
      events
    }
}

/** The device's screen for the connection numbered `owner`, until a newer one takes over. */
private class Screen(private val device: Device, private val owner: Long) {
  private val automation = device.automation

  fun waitFor(wanted: List<Wanted>, timeoutMs: Long): Look =
    await(timeoutMs, { "none of $wanted" }) { infos, screen ->
      wanted.withIndex().firstNotNullOfOrNull { (which, one) ->
        screen.firstOrNull(one::matches)?.let { Look(Found(which.toUInt(), it, screen), infos) }
      }
    }

  fun waitGone(wanted: List<Wanted>, timeoutMs: Long) =
    await(timeoutMs, { "still one of $wanted" }) { _, screen ->
      Unit.takeIf { screen.none { node -> wanted.any { it.matches(node) } } }
    }

  /** Taps (or long-presses) node `handle` of the last answered screen at its current bounds, with a
   * real touch. Throws if the node is gone or another window is in front at that point: a
   * keyboard, dialog or snackbar over it would take the touch. Not accessibility's ACTION_CLICK:
   * Signal's Compose nodes answer it with false. */
  fun tap(last: List<AccessibilityNodeInfo>, handle: Int, long: Boolean) {
    val node = requireNotNull(last.getOrNull(handle)) { "no node $handle" }
    check(node.refresh()) { "node $handle went" }
    val (x, y) = bounds(node).let { it.centerX() to it.centerY() }
    // Fresh, as the looks are: a keyboard that just came isn't in the cached list. Top first, as
    // `windows()` reads them; where touches land (a non-touchable overlay's region isn't).
    automation.clearCache()
    val front = automation.windows.firstOrNull { Region().also(it::getRegionInScreen).contains(x, y) }
    check(front == null || front.id == node.windowId) { "node $handle is under the window ${front?.title} at ($x, $y)" }
    shell(if (long) "input swipe $x $y $x $y $LONG_PRESS_MS" else "input tap $x $y")
  }

  /** Sets the input-focused field's text once a field has focus (accessibility's ACTION_SET_TEXT:
   * no keys, so nothing goes astray while the keyboard binds to it), then waits for it to read
   * as `echo` says. */
  fun setText(text: String, echo: Echo, timeoutMs: Long) {
    val deadline = SystemClock.uptimeMillis() + timeoutMs
    val field = awaitFocusedField(deadline) { true }
    val arguments =
      Bundle().apply {
        putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text)
      }
    val took = device.asOwner(owner) { field.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, arguments) }
    check(took) { "the focused field took no text" }
    awaitFocusedField(deadline) { echo.matches(it) }
  }

  /** Swipes up the largest list of the topmost window that has one until `label` shows. Each look
   * waits, as [waitFor] does, for a settled screen with the label or with a list offering to scroll
   * on ([SCROLLS]); a list stops offering it at its end, so there it times out. After each swipe it
   * waits for an event. Not the scroll action itself: Signal's Compose settings list offers it and
   * doesn't move. */
  fun scrollTo(label: String, timeoutMs: Long): Look {
    val deadline = SystemClock.uptimeMillis() + timeoutMs
    while (true) {
      val step =
        await(left(deadline), { "neither \"$label\" nor a list that scrolls on to it" }) { infos, screen ->
          screen.firstOrNull { it.labelled(label) }?.let { ScrollStep.Shown(Look(Found(0u, it, screen), infos)) }
            ?: infos
              .groupBy { it.windowId }
              .values
              .firstNotNullOfOrNull { window ->
                window
                  .filter { info -> info.actionList.any { it in SCROLLS } }
                  .maxByOrNull { info -> bounds(info).let { it.width() * it.height() } }
              }?.let { ScrollStep.Swipe(bounds(it)) }
        }
      when (step) {
        is ScrollStep.Shown -> return step.look
        is ScrollStep.Swipe -> {
          val seen = device.events()
          shell(swipeUp(step.list))
          // A list at its end doesn't move, and says nothing: it may have got there after the
          // look before (a fling the app's stalls made look settled), so look again.
          nextChange(seen, minOf(deadline, SystemClock.uptimeMillis() + SWIPE_STILL_MS))
          if (SystemClock.uptimeMillis() >= deadline) {
            throw TimeoutException("no \"$label\" within $timeoutMs ms")
          }
        }
      }
    }
  }

  /** The input-focused field, once there is one that `reads` as wanted, looked at now and after
   * each event. Not settled: it is typed into, not tapped. */
  private fun awaitFocusedField(
    deadline: Long,
    reads: (AccessibilityNodeInfo) -> Boolean,
  ): AccessibilityNodeInfo {
    var seen = -1L
    while (true) {
      seen =
        nextChange(seen, deadline)
          ?: throw TimeoutException("no focused field as wanted: ${shown(nodes(windows()))}")
      automation.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)?.takeIf { it.isEditable && reads(it) }?.let {
        return it
      }
    }
  }

  /** `look`'s answer, looked for now and after every event since, on the settled screen. */
  private fun <T : Any> await(
    timeoutMs: Long,
    what: () -> String,
    look: (List<AccessibilityNodeInfo>, List<Node>) -> T?,
  ): T {
    val deadline = SystemClock.uptimeMillis() + timeoutMs
    var seen = -1L
    var last = emptyList<Node>()
    while (true) {
      seen =
        nextChange(seen, deadline)
          ?: throw TimeoutException("${what()} within $timeoutMs ms: ${shown(last)}")
      fun lookNow(): T? {
        val infos = windows()
        last = nodes(infos)
        return if (answeredNotResponding(infos, last)) null else look(infos, last)
      }
      if (lookNow() == null) {
        continue
      }
      settle(deadline)
      lookNow()?.let {
        return it
      }
    }
  }

  /** Taps Wait on the system's "<app> isn't responding" dialog (AOSP strings `anr_title`, `wait`)
   * if it shows; whether it did. Signal's main thread stalls on the emulator (its settings, its
   * camera) past even the 30 s of the driver's `ro.hw_timeout_multiplier`, and the dialog in front
   * takes what comes next; hidden (`hide_error_dialogs`), Android kills the app instead.
   * Called from [await]'s looks only. */
  private fun answeredNotResponding(infos: List<AccessibilityNodeInfo>, screen: List<Node>): Boolean {
    val title = screen.firstOrNull { it.text.endsWith(" isn't responding") } ?: return false
    val wait = screen.firstOrNull { it.window == title.window && it.labelled("Wait") } ?: return false
    Log.w(TAG, "answered \"${title.text}\" with Wait")
    tap(infos, wait.handle.toInt(), false)
    return true
  }

  private fun nextChange(seen: Long, deadline: Long): Long? = device.nextChange(owner, seen, deadline)

  /** Until no event has come for [QUIET_MS], or [SETTLE_MS] have passed: then as it is. */
  private fun settle(deadline: Long) {
    try {
      automation.waitForIdle(QUIET_MS, minOf(SETTLE_MS, left(deadline)))
    } catch (_: TimeoutException) {
      // Never quiet (an animation, a camera preview): the screen as it is, as uiautomator's dump.
    }
  }

  private fun left(deadline: Long) = maxOf(1L, deadline - SystemClock.uptimeMillis())

  /** Runs `command` as the shell does, until it is done; throws with what it wrote to stderr, if
   * anything (`input` reports its failures there, and exits 0). */
  private fun shell(command: String) {
    // Every tap and swipe comes here: none from a connection a newer one replaced.
    val errors =
      device.asOwner(owner) {
        val (stdout, stdin, stderr) = automation.executeShellCommandRwe(command)
        stdin.close()
        ParcelFileDescriptor.AutoCloseInputStream(stdout).use { it.readBytes() }
        ParcelFileDescriptor.AutoCloseInputStream(stderr).use { it.readBytes() }.decodeToString()
      }
    check(errors.isBlank()) { "$command: $errors" }
  }

  /** Every window's visible nodes, top window first: a popup's before what it covers. Read fresh
   * (`clearCache`): with its cache, UiAutomation keeps showing a screen Signal's Compose navigation
   * has left, whatever events it hears. */
  private fun windows(): List<AccessibilityNodeInfo> {
    automation.clearCache()
    val roots =
      automation.windows.mapNotNull { it.root }.ifEmpty { listOfNotNull(automation.rootInActiveWindow) }
    val found = mutableListOf<AccessibilityNodeInfo>()

    fun walk(info: AccessibilityNodeInfo) {
      if (info.isVisibleToUser) {
        found += info
      }
      for (at in 0 until info.childCount) {
        info.getChild(at)?.let(::walk)
      }
    }
    roots.forEach(::walk)
    return found
  }

  /** The nodes as the driver reads them, `handle` their place in `infos`, `window` their
   * window's place from the top; `y` 0 for a node partly above the screen's top. */
  // isChecked: getChecked(), its replacement, is API 36, and the emulator's is 35.
  @Suppress("DEPRECATION")
  private fun nodes(infos: List<AccessibilityNodeInfo>): List<Node> {
    val windows = infos.map { it.windowId }.distinct()
    return infos.mapIndexed { handle, info ->
      Node(
        handle = handle.toUInt(),
        text = info.text?.toString().orEmpty(),
        content_desc = info.contentDescription?.toString().orEmpty(),
        class_name = info.className?.toString().orEmpty(),
        checked = info.isChecked,
        enabled = info.isEnabled,
        editable = info.isEditable,
        window = windows.indexOf(info.windowId).toUInt(),
        y = bounds(info).centerY().coerceAtLeast(0).toUInt(),
      )
    }
  }

  private fun shown(screen: List<Node>) =
    screen
      .flatMap { listOf(it.text, it.content_desc) }
      .filter(String::isNotEmpty)
      .joinToString(" | ")
}

private fun bounds(info: AccessibilityNodeInfo) = Rect().also(info::getBoundsInScreen)

/** A swipe up the middle of `list`, from a fifth above its bottom to a fifth below its top. */
private fun swipeUp(list: Rect): String {
  val x = list.centerX()
  val margin = list.height() / 5
  return "input swipe $x ${list.bottom - margin} $x ${list.top + margin} 300"
}
