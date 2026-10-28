// Signal-Desktop in the box, driven the way its mock-server tests drive it
// (ts/test-mock/playwright.node.ts, ts/test-mock/helpers.node.ts), answering JSON lines on
// DESKTOP_DRIVER_SOCKET: `{"id":1,"method":"...","params":{...}}` →
// `{"id":1,"result":...}` or `{"id":1,"error":"..."}` (testbox/tools `drivers::Desktop`).
// Run with Deno rather than Node, so that the image's build type-checks it against Playwright's
// types, lints and formats it; Playwright's Electron launcher runs on Deno's Node compatibility.
// Playwright and Electron at Desktop's versions (deno.json, checked against Desktop's
// package.json by testbox/desktop/Containerfile).

import process from 'node:process';
import electronBinary from 'electron';
import { _electron as electron, type Locator, type Page } from 'playwright-core';

/** What Desktop's CI mode puts on the renderer's global (ts/CI.preload.ts). */
interface SignalCI {
  waitForEvent(event: string, options: { timeout: number }): Promise<unknown>;
  getPendingEventCount(event: string): number;
  setMediaPermissions(): Promise<void>;
  openSignalRoute(url: string): Promise<void>;
}

declare global {
  var SignalCI: SignalCI;
}

const DESKTOP = '/opt/signal-desktop';

const socketPath = Deno.env.get('DESKTOP_DRIVER_SOCKET');
if (!socketPath) {
  throw new Error('DESKTOP_DRIVER_SOCKET is not set');
}

// `with-display` runs the driver on a display of its own, which Electron inherits.
if (!Deno.env.get('WAYLAND_DISPLAY')) {
  throw new Error('WAYLAND_DISPLAY is not set: run under with-display');
}

// The server half from the box's keys (testbox/tools `provision`); the run's half here.
const config = {
  ...JSON.parse(Deno.readTextFileSync('/keys/desktop-config.json')),
  // SignalCI's events (`SignalCI.waitForEvent`), as bootstrap.node.ts sets.
  ciMode: 'full',
  storagePath: '/home/desktop/data',
  // As bootstrap.node.ts sets it (`Date.now() + MONTH`): NODE_ENV=production is the packaged-app
  // environment (ts/environment.std.ts `PackagedApp = 'production'`), where 0 means expired, and a
  // date more than 31 days ahead counts as expired too (ts/util/buildExpiration.std.ts).
  buildExpiration: Date.now() + 30 * 24 * 60 * 60 * 1000,
};

/** How long a step may wait, as the checks' (xtask/src/checks/mod.rs `PATIENCE_MS`): also what
 * Playwright waits without a timeout of its own, rather than its 30 s, which Desktop overruns
 * under heavy disk writes (its composer took longer than that to show after a send). */
const PATIENCE_MS = 120_000;

/** The window, once `launch` has started Desktop. */
let window: Page | undefined;

function page(): Page {
  if (!window) {
    throw new Error('Desktop runs only after `launch`');
  }
  return window;
}

/** Not at the driver's start: the pod starts every container at once, and Desktop gives up
 * linking for good when the server isn't there yet ("[Provisioner] stopping, reason=Only socket
 * failed"). The xtask calls it once the server serves (xtask/src/checks/setup.rs `linking_desktop`). */
async function launch(): Promise<void> {
  const app = await electron.launch({
    // The package's default export is the binary's path (its index.js).
    executablePath: electronBinary as unknown as string,
    // Chromium's sandbox needs a setuid helper or user namespaces, which the container lacks.
    // Chromium's fake microphone (a tone), and no prompt for it: the container has no sound.
    args: [
      `${DESKTOP}/ci.js`,
      '--no-sandbox',
      '--use-fake-device-for-media-stream',
      '--use-fake-ui-for-media-stream',
      // Shared memory in /tmp: the pod's /dev/shm is 64 MB, which crashes renderers.
      '--disable-dev-shm-usage',
      '--ozone-platform=wayland',
    ],
    env: { ...Deno.env.toObject(), SIGNAL_CI_CONFIG: JSON.stringify(config) },
    locale: 'en',
    timeout: 120_000,
  });
  app.process().stdout?.pipe(process.stdout);
  app.process().stderr?.pipe(process.stderr);
  window = await app.firstWindow();
  window.setDefaultTimeout(PATIENCE_MS);
  // The microphone, for voice messages: Desktop grants it only with `mediaPermissions` in its user
  // config (app/permissions.std.ts; Settings, "Allow access to the microphone"), which SignalCI
  // sets as Desktop's own tests do (ts/CI.preload.ts `setMediaPermissions`).
  await window.evaluate(() => globalThis.SignalCI.setMediaPermissions());
}

/** SignalCI's next `name` event, the oldest one it holds first (ts/CI.preload.ts `waitForEvent`). */
function signalCiEvent(name: string, timeoutMs: number): Promise<unknown> {
  return page().evaluate(
    ([event, timeout]) => globalThis.SignalCI.waitForEvent(event, { timeout }),
    [name, timeoutMs] as const,
  );
}

/** How many `name` events SignalCI holds that nobody has taken yet. */
function signalCiPending(name: string): Promise<number> {
  return page().evaluate((event) => globalThis.SignalCI.getPendingEventCount(event), name);
}

/** The newest `name` event: SignalCI hands out the oldest it holds first. */
async function newestSignalCiEvent(name: string, timeoutMs: number): Promise<unknown> {
  let event;
  do {
    event = await signalCiEvent(name, timeoutMs);
  } while ((await signalCiPending(name)) > 0);
  return event;
}

/** Takes the `name` events SignalCI holds, so that the next one waited for is new. */
async function drainSignalCi(name: string, timeoutMs: number): Promise<void> {
  while ((await signalCiPending(name)) > 0) {
    await signalCiEvent(name, timeoutMs);
  }
}

/** A tab of Desktop's left rail, by test id: a tab's name carries its badge (NavTabs.dom.tsx
 * `NavTabsItem--${id}`). */
function navTab(tab: 'Chats' | 'Stories' | 'Settings'): Promise<void> {
  return page().getByTestId(`NavTabsItem--${tab}`).click();
}

function leftPane(): Locator {
  return page().locator('#LeftPane');
}

function timeline(): Locator {
  return page().locator('.module-timeline__messages__container');
}

/** The open conversation's messages with `text`. */
function message(text: string): Locator {
  return timeline().locator('.module-message').filter({ hasText: text });
}

/** A downloaded picture (helpers.node.ts `getLoadedImagesInside`, Image.dom.tsx `data-loaded`). */
const LOADED_IMAGE = 'img.module-image__image[data-loaded="true"]';

function enabledComposition(): Locator {
  return page().locator(
    '.ConversationView__composition-area [data-testid=CompositionInput][data-enabled=true]',
  );
}

async function typeAndSend(text: string): Promise<void> {
  const composition = enabledComposition();
  await composition.waitFor();
  const composer = composition.locator('.ql-editor');
  await composer.fill(text);
  await composer.press('Enter');
}

/** The newest message with `text`, hovered: what shows its buttons ("React to Message",
 * "Reply to Message", "More actions"; TimelineMessage.dom.tsx). */
async function hovered(text: string): Promise<Locator> {
  const found = timeline()
    .locator('[data-testid]')
    .filter({ hasText: text })
    .filter({ has: page().getByRole('button', { name: 'More actions' }) })
    .last();
  await found.hover();
  return found;
}

/** Resolves to null, the methods' answer, once `locator` shows; rejects after `timeoutMs`. */
/** `text` as Desktop renders it: the names it inserts are wrapped in Unicode isolation marks
 * (U+2068 FIRST STRONG ISOLATE, U+2069 POP DIRECTIONAL ISOLATE), which may stand between any two
 * of its characters. `whole` matches all of an element's text, not a part. */
function rendered(text: string, whole: boolean): RegExp {
  const marks = '[\u2068\u2069]*';
  const pattern = [...text].map((character) => RegExp.escape(character)).join(marks);
  return new RegExp(whole ? `^${marks}${pattern}${marks}$` : pattern);
}

/** The open group's "Edit group" dialog, from its settings' header: its title, `old_title`. */
async function openEditGroup(old_title: string): Promise<Locator> {
  await openGroupSettings();
  await page()
    .locator('[data-testid=ConversationDetailsHeader]')
    .getByRole('button', { name: old_title })
    .click();
  return page().getByRole('dialog', { name: 'Edit group' });
}

/** The open group's settings: More Info, Group settings. */
async function openGroupSettings(): Promise<void> {
  await page()
    .locator('.Inbox__conversation-stack')
    .getByRole('button', { name: 'More Info' })
    .click();
  await page().getByRole('menuitem', { name: 'Group settings' }).click();
}

/** Chooses the avatar editor's first preset whose picture is drawn (its background set): one
 * clicked before is selected without a picture to save, and Save stays disabled
 * (AvatarEditor.dom.tsx `avatarBuffer`, BetterAvatar.dom.tsx `backgroundImage`). */
async function choosePreset(scope: Page | Locator): Promise<void> {
  await scope.locator('.AvatarEditor__avatars > [style*="url("]').first().click();
}

async function appears(locator: Locator, timeoutMs: number): Promise<null> {
  await locator.first().waitFor({ timeout: timeoutMs });
  return null;
}

/** helpers.node.ts `acceptConversation`, if the conversation is a message request. */
async function acceptIfRequest(): Promise<void> {
  // A request shows its bar, an accepted conversation an enabled composer: whichever comes.
  const actions = page().getByTestId('message-request-actions');
  await actions.or(enabledComposition()).first().waitFor();
  if (!(await actions.isVisible())) {
    return;
  }
  await actions.getByRole('button', { name: 'Accept' }).click();
  // Accepted, or asked to confirm first: whichever shows (the composer is enabled once accepted).
  const dialog = page().getByRole('alertdialog', { name: 'Accept request?' });
  await dialog.or(enabledComposition()).first().waitFor();
  if (await dialog.isVisible()) {
    await dialog.getByRole('button', { name: 'Accept' }).click();
  }
}

interface Text {
  text: string;
}
interface Timeout {
  timeout_ms: number;
}

/** The calls, by `drivers::Desktop`'s method names, with their params. */
const methods = {
  launch: async () => {
    if (window) {
      throw new Error('Desktop runs already');
    }
    await launch();
    return null;
  },
  /** The `sgnl://linkdevice` URL the link screen shows now. Desktop renews it whenever its
   * provisioning socket is closed, and SignalCI keeps every one, handing out the oldest first
   * (ts/CI.preload.ts `completedEvents`); only the newest has a socket behind it. */
  provisioning_url: () => newestSignalCiEvent('provisioning-url', 120_000),
  /** Resolves once Desktop shows its inbox. `app-loaded` comes earlier: after linking Desktop
   * waits for the primary's contact sync and the storage service (ts/background.preload.ts,
   * "Wait for critical post-registration syncs before showing inbox"), which presage-cli doesn't
   * answer, until those waits give up. */
  inbox: async ({ timeout_ms }: Timeout) => {
    await signalCiEvent('app-loaded', timeout_ms);
    await leftPane().waitFor({ timeout: timeout_ms });
    return null;
  },
  /** Opens the conversation with `aci` from the left pane (helpers.node.ts
   * `clickOnConversationWithAci`), accepting it if it is a message request. Returns once Desktop
   * has opened it, as its own tests wait (test-mock/playwright.node.ts `waitForConversationOpen`):
   * the open takes a while after the click, 25 s under heavy disk writes, and until then there is
   * no composer for a send. One already open is clicked all the same, as the checks' way to make
   * Desktop read (ActiveWindowService.std.ts); Desktop opens nothing then, and says nothing. */
  open_conversation: async ({ aci }: { aci: string }) => {
    const item = leftPane().getByTestId(aci);
    await item.waitFor();
    const open = ((await item.getAttribute('class')) ?? '').includes(
      'module-conversation-list__item--contact-or-conversation--is-selected',
    );
    // `conversation:open` names no conversation, and SignalCI hands out the oldest it holds.
    while ((await signalCiPending('conversation:open')) > 0) {
      await signalCiEvent('conversation:open', 1_000);
    }
    await item.click();
    if (!open) {
      await signalCiEvent('conversation:open', PATIENCE_MS);
    }
    await acceptIfRequest();
    return null;
  },
  send: async ({ text }: Text) => {
    await typeAndSend(text);
    return null;
  },
  /** Waits up to `timeout_ms` for a message with `text` in the open conversation. */
  wait_for_message: ({ text, timeout_ms }: Text & Timeout) => appears(message(text), timeout_ms),
  /** Sends `paths` with `text` as its caption, as helpers.node.ts `composerAttachFiles` and
   * `sendMessageWithAttachments` do. */
  send_files: async ({ paths, text }: Text & { paths: string[] }) => {
    await page().getByRole('button', { name: 'Add attachment or poll' }).click();
    await page().getByRole('menuitem', { name: 'File' }).click();
    await page().getByTestId('attachfile-input').setInputFiles(paths);
    await page().locator('.module-attachments__rail').waitFor();
    await typeAndSend(text);
    return null;
  },
  /** Waits for the message with `text` to show a downloaded picture (helpers.node.ts
   * `getLoadedImagesInside`). */
  wait_for_image: ({ text, timeout_ms }: Text & Timeout) =>
    appears(message(text).locator(LOADED_IMAGE), timeout_ms),
  /** Waits for a file attachment named `file_name` (Message.dom.tsx `simple-attachment`). */
  wait_for_file: ({ file_name, timeout_ms }: Timeout & { file_name: string }) =>
    appears(
      timeline()
        .locator('.module-message__simple-attachment__file-name')
        .filter({ hasText: file_name }),
      timeout_ms,
    ),
  /** Records a voice message of a second or more from the fake microphone and sends it
   * (AudioCapture.dom.tsx "Start recording voice message", RecordingComposer.dom.tsx "Send"). */
  send_voice_message: async () => {
    await page().getByRole('button', { name: 'Start recording voice message' }).click();
    // The timer counts the recording (CompositionRecording.dom.tsx); 0:00 would send nothing.
    await page()
      .locator('.CompositionRecording__timer')
      .filter({ hasText: /^0:0[1-9]$/ })
      .waitFor();
    await page().locator('.RecordingComposer').getByRole('button', { name: 'Send' }).click();
    return null;
  },
  /** Waits for an incoming audio or voice message (MessageAudio.dom.tsx
   * `module-message__audio-attachment--incoming`). */
  wait_for_incoming_audio: ({ timeout_ms }: Timeout) =>
    appears(timeline().locator('.module-message__audio-attachment--incoming'), timeout_ms),
  /** Waits for a sticker, its picture loaded (Message.dom.tsx `module-message__sticker-container`,
   * Image.dom.tsx `data-loaded`). */
  wait_for_sticker: ({ timeout_ms }: Timeout) =>
    appears(
      timeline().locator(`.module-message__sticker-container ${LOADED_IMAGE}`),
      timeout_ms,
    ),
  /** Waits for the message with `text` to show as delivered or read
   * (ts/components/conversation/MessageMetadata.dom.tsx status icons). */
  wait_for_delivered: ({ text, timeout_ms }: Text & Timeout) =>
    appears(
      message(text).locator(
        '.module-message__metadata__status-icon--delivered, .module-message__metadata__status-icon--read',
      ),
      timeout_ms,
    ),
  /** Waits for the open conversation's own message `text` to show `status` exactly
   * (`.module-message__metadata__status-icon--{status}`, MessageMetadata.dom.tsx): after an edit,
   * Desktop shows only what the receipts for the edit say (MessageReceipts.preload.ts
   * `updateMessageSendStateWithReceipt`). */
  wait_for_status: ({ text, status, timeout_ms }: Text & Timeout & { status: string }) =>
    appears(
      message(text).locator(`.module-message__metadata__status-icon--${status}`),
      timeout_ms,
    ),
  /** Waits for `text` in bold (`<strong>`, MessageTextRenderer.scss) in the open conversation. */
  wait_for_bold: ({ text, timeout_ms }: Text & Timeout) =>
    appears(
      timeline().locator('.module-message__text strong').filter({ hasText: text }),
      timeout_ms,
    ),
  /** Edits the message reading `old_text` to `text`, as edit_test.node.ts `sendEditedMessage`
   * does: the message's "More actions" menu, then "Edit", then the composer. */
  edit_message: async ({ old_text, text }: Text & { old_text: string }) => {
    const edited = await hovered(old_text);
    await edited.getByRole('button', { name: 'More actions' }).click();
    await page().getByRole('menuitem', { name: 'Edit' }).click();
    await typeAndSend(text);
    return null;
  },
  /** Reacts to the message with `text` with the first of the picker's emoji (ReactionPicker.dom.tsx,
   * the account's preferred reactions; their buttons have no name). */
  react_to_message: async ({ text }: Text) => {
    const reacted = await hovered(text);
    await reacted.getByRole('button', { name: 'React to Message' }).click();
    await page().locator('.module-ReactionPickerPicker__button--emoji').first().click();
    return null;
  },
  /** Waits for a reaction on the message with `text` (Message.dom.tsx
   * `module-message__reactions__reaction`). */
  wait_for_reaction: ({ text, timeout_ms }: Text & Timeout) =>
    appears(message(text).locator('.module-message__reactions__reaction'), timeout_ms),
  /** Sends `text` as a reply to the message with `quoted`, from its "Reply to Message" button. */
  reply_to_message: async ({ quoted, text }: Text & { quoted: string }) => {
    const replied = await hovered(quoted);
    await replied.getByRole('button', { name: 'Reply to Message' }).click();
    await typeAndSend(text);
    return null;
  },
  /** Waits for the message with `text` to quote one with `quoted` (Quote.dom.tsx
   * `module-quote__primary__text`). */
  wait_for_quote: ({ text, quoted, timeout_ms }: Text & Timeout & { quoted: string }) =>
    appears(
      message(text).locator('.module-quote__primary__text').filter({ hasText: quoted }),
      timeout_ms,
    ),
  /** Deletes the message with `text` for everyone: its "More actions" menu's "Delete", then
   * DeleteMessagesModal.dom.tsx's button. Not its context menu, as attachments_test.node.ts
   * does: on Wayland a right-click opens none. */
  delete_for_everyone: async ({ text }: Text) => {
    const deleted = await hovered(text);
    await deleted.getByRole('button', { name: 'More actions' }).click();
    await page().getByRole('menuitem', { name: 'Delete' }).click();
    await page().getByRole('button', { name: 'Delete for everyone' }).click();
    return null;
  },
  /** Sets the account's profile picture to Desktop's first preset avatar (Settings, Edit photo,
   * AvatarEditor.dom.tsx `AvatarEditor__avatars`), which it uploads like a photo; then back to
   * the chats. */
  set_profile_avatar: async () => {
    await navTab('Settings');
    await page().getByText('Edit photo').click();
    await choosePreset(page());
    await page().getByRole('button', { name: 'Save', exact: true }).click();
    await page().getByText('Edit photo').waitFor();
    await navTab('Chats');
    return null;
  },
  /** Opens the conversation the left pane lists as `title`, accepting it if it is a request: by a
   * row whose name is exactly that, not by its whole text, which has the last message in it. */
  open_titled: async ({ title }: { title: string }) => {
    const name = rendered(title, true);
    try {
      await leftPane().getByText(name).first().click();
    } catch (error) {
      // The rows the left pane lists, for the error.
      const listed = await leftPane().locator('[data-testid]').allInnerTexts();
      throw new Error(
        `no row named ${JSON.stringify(title)} in ${JSON.stringify(listed)}: ${error}`,
      );
    }
    await acceptIfRequest();
    return null;
  },
  /** Waits for `text` in the open conversation, a notice (`.SystemMessage`) included. */
  wait_for_text: ({ text, timeout_ms }: Text & Timeout) =>
    appears(timeline().getByText(rendered(text, false)), timeout_ms),
  /** Gives the open group the first of Desktop's preset pictures: "Edit group", its camera
   * (AvatarPreview `__upload`), the first preset ([choosePreset]), the editor's Save, back to
   * the group's fields, then theirs. */
  set_group_picture: async ({ old_title }: { old_title: string }) => {
    const dialog = await openEditGroup(old_title);
    await dialog.locator('.AvatarPreview__upload').click();
    await choosePreset(dialog);
    await dialog.getByRole('button', { name: 'Save' }).click();
    await dialog.locator('.AvatarEditor__avatars').waitFor({ state: 'hidden' });
    await dialog.getByRole('button', { name: 'Save' }).click();
    await dialog.waitFor({ state: 'hidden' });
    await page().keyboard.press('Escape');
    return null;
  },
  /** Sends `text` after a mention of `member`: "@" and the name typed into the composer, its
   * suggestion shown (CompositionInput's `module-composition-input__suggestions__row--mention`),
   * Enter taking the highlighted one, the first. */
  send_mention: async ({ member, text }: { member: string; text: string }) => {
    const composition = enabledComposition();
    await composition.waitFor();
    const composer = composition.locator('.ql-editor');
    await composer.pressSequentially(`@${member}`);
    await page()
      .locator('.module-composition-input__suggestions__row--mention')
      .filter({ hasText: rendered(member, false) })
      .waitFor();
    await composer.press('Enter');
    await composer.pressSequentially(` ${text}`);
    await composer.press('Enter');
    return null;
  },
  /** Waits for a story with `text` posted to the group `from`: its row in the Stories tab
   * (StoryListItem), opened. */
  story_shows: async (
    { from, text, timeout_ms }: { from: string; text: string; timeout_ms: number },
  ) => {
    await navTab('Stories');
    try {
      await page()
        .locator('.StoryListItem')
        .filter({ hasText: rendered(from, false) })
        .first()
        .click({ timeout: timeout_ms });
      return await appears(page().getByText(rendered(text, false)), timeout_ms);
    } finally {
      await page().keyboard.press('Escape');
      await navTab('Chats');
    }
  },
  /** Leaves the open group, from its settings (messages.json
   * `icu:ConversationDetailsActions--leave-group`, `--leave-group-modal-confirm`). */
  leave_group: async () => {
    await openGroupSettings();
    await page().getByRole('button', { name: 'Leave group' }).click();
    // An AxoConfirmDialog: an alert dialog, as Desktop's own tests find them (helpers.node.ts).
    const dialog = page().getByRole('alertdialog', { name: 'Do you really want to leave?' });
    await dialog.getByRole('button', { name: 'Leave', exact: true }).click();
    await dialog.waitFor({ state: 'hidden' });
    await page().keyboard.press('Escape');
    return null;
  },
  /** Opens `link` as a click on it does (SignalCI `openSignalRoute`), then asks to join or joins
   * in the dialog it opens (messages.json `icu:GroupV2--join--request-to-join-button`,
   * `icu:GroupV2--join--join-button`). */
  join_group: async ({ link }: { link: string }) => {
    await page().evaluate((url) => globalThis.SignalCI.openSignalRoute(url), link);
    await page()
      .getByRole('button', { name: /^(Request to Join|Join)$/ })
      .click();
    return null;
  },
  /** Edits the open group's name, description or both, as send_gv2_invite_test.node.ts renames
   * one: More Info, Group settings, the header's title, then "Edit group" (its fields by their
   * placeholders, EditConversationAttributesModal.dom.tsx). Back to the conversation after. */
  edit_group: async (
    { old_title, title, description }: {
      old_title: string;
      title: string | null;
      description: string | null;
    },
  ) => {
    const dialog = await openEditGroup(old_title);
    if (title !== null) {
      await dialog.getByRole('textbox', { name: 'Group name (required)' }).fill(title);
    }
    if (description !== null) {
      await dialog.getByRole('textbox', { name: 'Description' }).fill(description);
    }
    await dialog.getByRole('button', { name: 'Save' }).click();
    await dialog.waitFor({ state: 'hidden' });
    await page().keyboard.press('Escape');
    return null;
  },
  /** Turns read receipts on or off (Settings, Privacy, as settings_test.node.ts opens it), and
   * resolves once Desktop has written the account record to the storage service with it
   * (storageRecordOps.preload.ts `readReceipts`; storage.preload.ts `uploadManifest`); then back
   * to the chats. */
  set_read_receipts: async ({ on, timeout_ms }: Timeout & { on: boolean }) => {
    // Earlier uploads' events: SignalCI hands out the oldest first.
    await drainSignalCi('uploadManifest', timeout_ms);
    await navTab('Settings');
    await page().getByRole('button', { name: 'Privacy' }).click();
    const toggle = page()
      .getByRole('listitem', { name: 'Read receipts', exact: true })
      .getByRole('switch');
    if ((await toggle.getAttribute('aria-checked')) === String(on)) {
      throw new Error(`read receipts are ${on ? 'on' : 'off'} already`);
    }
    await toggle.click();
    await signalCiEvent('uploadManifest', timeout_ms);
    await navTab('Chats');
    return null;
  },
} satisfies Record<string, (params: never) => Promise<unknown>>;

type Method = keyof typeof methods;

function isMethod(name: unknown): name is Method {
  return typeof name === 'string' && Object.hasOwn(methods, name);
}

/** Answers one request line; the reply line. */
async function answer(line: string): Promise<string> {
  let id: unknown = null;
  try {
    const request: { id?: unknown; method?: unknown; params?: unknown } = JSON.parse(line);
    id = request.id ?? null;
    const method = request.method;
    if (!isMethod(method)) {
      throw new Error(`no method ${method}`);
    }
    // The params are as `drivers::Desktop` serializes them for this method.
    const call = methods[method] as (params: unknown) => Promise<unknown>;
    const result = await call(request.params ?? {});
    return JSON.stringify({ id, result });
  } catch (error) {
    return JSON.stringify({ id, error: String(error) });
  }
}

/** One request at a time, in order, across connections: the window is one. */
let queue = Promise.resolve();

async function serve(connection: Deno.UnixConn): Promise<void> {
  const encoder = new TextEncoder();
  let buffered = '';
  for await (const chunk of connection.readable.pipeThrough(new TextDecoderStream())) {
    buffered += chunk;
    let newline;
    while ((newline = buffered.indexOf('\n')) >= 0) {
      const line = buffered.slice(0, newline);
      buffered = buffered.slice(newline + 1);
      const replied = queue.then(() => answer(line));
      // A reply that can't be written is that connection's loss, not the next request's.
      queue = replied
        .then(async (reply) => {
          await connection.write(encoder.encode(`${reply}\n`));
        })
        .catch((error) => console.error(`desktop driver: answering: ${error}`));
    }
  }
}

// As the container's first process, SIGTERM is ours to act on: the pod is stopping.
Deno.addSignalListener('SIGTERM', () => Deno.exit(0));

try {
  Deno.removeSync(socketPath);
} catch (error) {
  if (!(error instanceof Deno.errors.NotFound)) {
    throw error;
  }
}
const listener = Deno.listen({ transport: 'unix', path: socketPath });
// The owner's only, as b33p's harness socket is (b33p-slint/src/agent.rs): the socket's directory
// is on the host.
Deno.chmodSync(socketPath, 0o600);
console.log(`driver listening on ${socketPath}`);
for await (const connection of listener) {
  // A connection that fails ends; the driver serves the next.
  serve(connection).catch((error) => console.error(`desktop driver: ${error}`));
}
