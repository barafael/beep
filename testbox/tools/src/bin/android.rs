//! Signal-Android's `local` flavor in the box.
//!
//! Boots the emulator and installs the APK the image built for the box's keys (Containerfile
//! stage `app`), and answers the xtask's calls (`testbox_tools::drivers`) on
//! ANDROID_DRIVER_SOCKET, one at a time: the screen is one.
//!
//! Every wait is on an event: the screen's through the on-device agent (testbox/android/agent),
//! the app's through its own log lines (logcat), as Desktop's through SignalCI's events, the
//! boot's through the emulator's "Boot completed", `adb wait-for-device` and the agent's word that
//! it listens. A dialog that may or may not come is waited for together with what comes without
//! it. A wait's upper bound only turns a hang into a failure that says what the screen showed.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, LineWriter, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Condvar, Mutex, MutexGuard, mpsc};
use std::time::Duration;

use serde::Serialize;
use strum::VariantArray;
use testbox_tools::drivers::{self, Android, Answer, ChatMenuItem, MemberChange, Numbered};
use testbox_tools::keys::{self, DesktopConfig, KeyFile};
use testbox_tools::process::output;
use testbox_tools::{
    Context, Describe, Error, Result, WithContext, env_var, read, remove_if_present, write,
};
use testbox_tools::{agent, avd};

/// What `android config` writes for the image's `app` stage: the local flavor's values
/// (testbox/android/Containerfile).
const CONFIG_OUT: &str = "/out";
const CONFIG: &str = "local.json";
/// The APK the image keeps, built for the box's keys, and the configuration it was built with.
const APK: &str = "/opt/testbox/signal.apk";
const APK_CONFIG: &str = "/opt/testbox/signal.apk.json";
const LOGCAT: &str = "/tmp/logcat.txt";
const QR: &str = "/tmp/qr.png";
/// What the emulator prints once Android has booted.
const BOOTED: &str = "Boot completed";
/// Android's ANR timeouts, times this ([`raise_anr_timeouts`]).
const HW_TIMEOUT_MULTIPLIER: u32 = 6;
/// Android's event log, once a framework start has unlocked the user (UserController's event
/// tag 30072): before it, the screen is on but the user locked, and Signal's activities don't
/// resolve ("No activity found").
const FRAMEWORK_STARTED: &str = "uc_finish_user_unlocked_completed";
const PACKAGE_PREFIX: &str = "org.thoughtcrime.securesms";
/// How long a step waits for what it wants, unless it takes [`SLOW`].
const PATIENCE: Duration = Duration::from_secs(30);
const SLOW: Duration = Duration::from_secs(60);
/// The on-device agent (testbox/android/agent), its instrumentation, and the socket it listens on,
/// which `am instrument -r` prints once it does (Agent.kt `sendStatus`).
const AGENT_APK: &str = "/opt/testbox/agent.apk";
const AGENT_INSTRUMENTATION: &str = "org.b33p.testbox.agent/.Agent";
const AGENT_ABSTRACT: &str = "b33p-testbox-agent";
/// adb's end of the agent's socket, forwarded to it.
const AGENT_SOCKET: &str = "/tmp/agent.sock";
/// What StorageSyncJob.kt logs once it has taken in a newer manifest.
const STORAGE_SYNCED: &str = "[Remote Sync] Saved new manifest";

fn main() -> std::process::ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match arguments.as_slice() {
        [] => start(),
        [command] if command == "config" => config(),
        [command, metadata, group, version, out] if command == "verification" => {
            verification(Path::new(metadata), Path::new(group), version, Path::new(out))
        }
        // Beside a running driver, which serves one connection at a time: `cargo testbox qr`.
        [show, url] if show == "show-qr" => show_qr(url),
        _ => Err(Error::Failed(
            "usage: android [config | verification <metadata> <group> <version> <out> | show-qr <url>]"
                .to_owned(),
        )),
    };
    testbox_tools::exit("android", outcome)
}

/// The image's `config` stage: the local flavor's values the APK is built with for the box's keys
/// (testbox/android/Containerfile; the keys come from the server image). The same bytes
/// for the same keys, so that Gradle builds again only for new keys.
fn config() -> Result<()> {
    let (_config, text) = local_config_json()?;
    let out = Path::new(CONFIG_OUT);
    std::fs::create_dir_all(out).context(CONFIG_OUT)?;
    write(&out.join(CONFIG), text)
}

/// The image's `verification` stage: Signal-Android's `metadata` with the patched libsignal's
/// artifacts in (`testbox_tools::verification`), from `group` (a Maven repository's
/// `org/signal`) at `version`, into `out`. The same bytes for the same artifacts, so that Gradle
/// builds again only for new ones.
fn verification(metadata: &Path, group: &Path, version: &str, out: &Path) -> Result<()> {
    let components = testbox_tools::verification::components(group, version)?;
    let patched =
        testbox_tools::verification::with_components(&read(metadata)?, version, &components)?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).context(dir.display().to_string())?;
    }
    write(out, patched)
}

fn start() -> Result<()> {
    let socket = testbox_tools::env_var("ANDROID_DRIVER_SOCKET")?;
    let (config, text) = local_config_json()?;
    if read(Path::new(APK_CONFIG))? != text {
        return Err(Error::Failed(format!(
            "the image's APK was built for other keys than the box's ({APK_CONFIG}): build the \
             android image after the server image, which holds the keys"
        )));
    }
    boot_emulator(&config)?;
    let launcher = install(Path::new(APK))?;
    let mut driver = Driver {
        launcher,
        agent: Agent::start()?,
    };
    no_soft_keyboard()?;
    driver.launch()?;
    serve(&mut driver, Path::new(&socket))
}

/// The `local` flavor's values (testbox/patches/signal-android-local.patch reads them as the
/// Gradle property `signalLocalConfig`).
#[derive(Serialize)]
struct LocalConfig {
    service_url: String,
    storage_url: String,
    cdn0_url: String,
    cdn2_url: String,
    cdn3_url: String,
    chat_port: u16,
    root_certificate_der: String,
    unidentified_sender_trust_root: String,
    zkgroup_server_public_params: String,
    generic_server_public_params: String,
    backup_server_public_params: String,
    captcha_url: String,
}

/// Desktop's values (testbox/tools `provision`), with the omnibus port for the chat: Desktop's
/// URLs name Jetty's HTTP/2 port, which the app's REST requests take too.
fn local_config() -> Result<LocalConfig> {
    let desktop_path = KeyFile::Desktop.path();
    let desktop: DesktopConfig =
        serde_json::from_str(&read(&desktop_path)?).context(desktop_path.display().to_string())?;
    let der = keys::pem_body(&read(&KeyFile::TlsCa.path())?, "CERTIFICATE")
        .ok_or_else(|| Error::Failed(format!("{} has no certificate", KeyFile::TlsCa)))?;
    Ok(LocalConfig {
        service_url: desktop.server_url,
        storage_url: desktop.storage_url,
        cdn0_url: desktop.cdn.cdn0,
        cdn2_url: desktop.cdn.cdn2,
        cdn3_url: desktop.cdn.cdn3,
        chat_port: keys::OMNIBUS_PORT,
        root_certificate_der: der,
        unidentified_sender_trust_root: desktop
            .server_trust_roots
            .into_iter()
            .next()
            .ok_or_else(|| Error::Failed("desktop-config.json has no trust root".to_owned()))?,
        zkgroup_server_public_params: desktop.server_public_params,
        generic_server_public_params: desktop.generic_server_public_params,
        backup_server_public_params: desktop.backup_server_public_params,
        // The box's captcha page hands the app the captcha the server takes, the way
        // signalcaptchas.org does: by navigating to the scheme the app intercepts
        // (CaptchaFragment.kt `shouldOverrideUrlLoading`).
        captcha_url: format!(
            "data:text/html,{}",
            percent_encoded(&format!(
                "<script>location.href='{}'</script>",
                testbox_tools::CAPTCHA
            ))
        ),
    })
}

/// The local configuration, and its JSON as the image's APK was built with it.
fn local_config_json() -> Result<(LocalConfig, String)> {
    let config = local_config()?;
    let text = serde_json::to_string_pretty(&config).context("the local configuration")?;
    Ok((config, text))
}

/// Every byte but RFC 3986's unreserved ones as `%XX`.
fn percent_encoded(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// What adb and the device say of themselves, to the log: after adb failed in a way that doesn't
/// explain itself. Each part as far as it goes.
fn print_device_state() {
    let parts: [&[&str]; 5] = [
        &["devices", "-l"],
        &["shell", "getprop", "sys.boot_completed"],
        &["shell", "getprop", "init.svc.adbd"],
        &["shell", "id"],
        &["logcat", "-d", "-t", "300"],
    ];
    for part in parts {
        match adb(part) {
            Ok(said) => println!("adb {}:\n{said}", part.join(" ")),
            Err(error) => println!("adb {}: {error}", part.join(" ")),
        }
    }
}

/// How often `adb root` is tried, each time once the device is back.
const ROOT_TRIES: u32 = 5;

/// `adb root`, once adb reaches the device. Right after boot the emulator's transport can go
/// offline for a moment (`adb devices`: `offline`; the emulator's own `adb shell` fails then too),
/// and `adb root` answers "unable to connect for root: closed": tried again once it is back.
fn root() -> Result<()> {
    let mut tries = 0;
    loop {
        // `wait-for-device` waits for the `device` state, past `offline`.
        adb(&["wait-for-device"])?;
        match adb(&["root"]) {
            Ok(_) => return Ok(()),
            Err(error) if tries + 1 < ROOT_TRIES => {
                tries += 1;
                println!("android: adb root: {error}; again once the device is back");
            }
            Err(error) => {
                // What the device says, for the next time.
                print_device_state();
                return Err(error);
            }
        }
    }
}

fn adb(args: &[&str]) -> Result<String> {
    let mut command = Command::new("adb");
    command.args(args);
    output(command, "")
}

/// Multiplies Android's ANR timeouts (`Build.HW_TIMEOUT_MULTIPLIER`; `InputConstants`
/// `DEFAULT_DISPATCHING_TIMEOUT_MILLIS`: input 5 s to 30 s), so Signal's stalls on the emulator
/// bring no "isn't responding" dialog. `ro.hw_timeout_multiplier` is read-only once set and only
/// vendor_init may set it, but this image leaves it unset and makes adb's root shell permissive
/// (its sepolicy: `(typepermissive su)`); the emulator's `-prop` takes only `qemu.*`. The
/// framework reads it as it starts, so this restarts it.
fn raise_anr_timeouts() -> Result<()> {
    root()?;
    adb(&["wait-for-device"])?;
    adb(&[
        "shell",
        "setprop",
        "ro.hw_timeout_multiplier",
        &HW_TIMEOUT_MULTIPLIER.to_string(),
    ])?;
    let multiplier = adb(&["shell", "getprop", "ro.hw_timeout_multiplier"])?;
    if multiplier.trim().parse() != Ok(HW_TIMEOUT_MULTIPLIER) {
        return Err(Error::Failed(format!(
            "ro.hw_timeout_multiplier is {multiplier:?}, not {HW_TIMEOUT_MULTIPLIER}"
        )));
    }
    adb(&["shell", "stop"])?;
    // Only this start's lines: the first boot's are cleared.
    adb(&["logcat", "-b", "events", "-c"])?;
    let mut events = Command::new("adb")
        .args(["logcat", "-b", "events"])
        .stdout(Stdio::piped())
        .spawn()
        .context("adb logcat -b events")?;
    let stdout = events
        .stdout
        .take()
        .ok_or_else(|| Error::Failed("the event log's stdout isn't piped".to_owned()))?;
    let started = announced(stdout, "Android's event log", "events: ", FRAMEWORK_STARTED);
    adb(&["shell", "start"])?;
    let heard = started
        .recv()
        .map_err(|_| Error::Failed("the event log ended before the framework started".to_owned()));
    events.kill().context("adb logcat -b events")?;
    events.wait().context("adb logcat -b events")?;
    heard?;
    adb(&["unroot"])?;
    adb(&["wait-for-device"]).map(drop)
}

/// The AVD's locks from an emulator of this container before it restarted (`restartPolicy:
/// Always`): its processes went with it, and the emulator refuses an AVD it finds locked
/// ("Running multiple emulators with the same AVD").
fn clear_avd_locks(avd: &Path) -> Result<()> {
    for entry in std::fs::read_dir(avd).context(avd.display().to_string())? {
        let path = entry.context(avd.display().to_string())?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "lock")
        {
            remove_lock(&path)?;
        }
    }
    Ok(())
}

/// A lock is a file or, for the snapshot's, a directory.
fn remove_lock(path: &Path) -> Result<()> {
    let removed = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.context(path.display().to_string())
}

/// `url`'s QR on the wall poster in the camera's scene (testbox/android/Toren1BD.posters).
fn show_qr(url: &str) -> Result<()> {
    let mut qrencode = Command::new("qrencode");
    qrencode.args(["-s", "10", "-m", "4", "-o", QR, "--", url]);
    output(qrencode, "")?;
    adb(&["emu", "virtualscene-image", "wall", QR]).map(drop)
}

/// The port of a URL on the clients' localhost.
fn localhost_port(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("https://localhost:")?;
    rest.split(|character: char| !character.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// The AVD's hardware for the box ([`avd::Config::for_box`]), at the display's size.
fn configure_avd(avd: &Path) -> Result<()> {
    let path = avd.join("config.ini");
    let mut config = avd::Config::parse(&read(&path)?);
    config.for_box(&env_var(testbox_tools::DISPLAY_SIZE)?)?;
    write(&path, config.to_string())
}

/// The device at the scale the AVD's user config names, which the emulator caps at 80% of the
/// display (its default leaves most of the display black). Read at start
/// (`user_config_get_window_scale`), written at exit.
fn set_window_scale(avd: &Path) -> Result<()> {
    let user_config = avd.join("emulator-user.ini");
    write(
        &user_config,
        "window.x = 0\nwindow.y = 0\nwindow.scale = 1.0\n",
    )
}

fn boot_emulator(config: &LocalConfig) -> Result<()> {
    let avd = Path::new(&env_var("HOME")?).join(".android/avd/box.avd");
    clear_avd_locks(&avd)?;
    configure_avd(&avd)?;
    set_window_scale(&avd)?;
    // The adb server first, so it finds the emulator as it comes up.
    adb(&["start-server"])?;
    let mut emulator = Command::new("emulator")
        .args(["-avd", "box", "-no-audio", "-no-boot-anim", "-no-snapshot"])
        .args([
            "-no-metrics",
            "-gpu",
            "swiftshader_indirect",
            "-memory",
            "3072",
        ])
        .args(["-camera-back", "virtualscene"])
        .stdout(Stdio::piped())
        .spawn()
        .context("the emulator")?;
    let stdout = emulator
        .stdout
        .take()
        .ok_or_else(|| Error::Failed("the emulator's stdout isn't piped".to_owned()))?;
    let boot = announced(stdout, "the emulator's output", "", BOOTED);
    // The channel closes when the emulator's output ends: it exited.
    boot.recv().map_err(|_| match emulator.try_wait() {
        Ok(Some(status)) => Error::Failed(format!("the emulator exited while booting: {status}")),
        Ok(None) => Error::Failed("the emulator's output ended while booting".to_owned()),
        Err(error) => Error::Io {
            what: "the emulator, whose output ended while booting".to_owned(),
            source: error,
        },
    })?;
    adb(&["wait-for-device"])?;
    let completed = adb(&["shell", "getprop", "sys.boot_completed"])?;
    if completed.trim() != "1" {
        return Err(Error::Failed(format!(
            "the emulator says it booted, Android's sys.boot_completed {completed:?}"
        )));
    }
    raise_anr_timeouts()?;

    // The guest's localhost is its own; the servers are on the pod's, the emulator process's.
    let urls = [
        &config.service_url,
        &config.storage_url,
        &config.cdn0_url,
        &config.cdn2_url,
        &config.cdn3_url,
    ];
    let ports: BTreeSet<u16> = urls
        .into_iter()
        .filter_map(|url| localhost_port(url))
        .chain([config.chat_port])
        .collect();
    for port in ports {
        let tcp = format!("tcp:{port}");
        adb(&["reverse", &tcp, &tcp])?;
    }
    // A new message's notification stays in the shade instead of popping over the app's top for
    // seconds (`Settings.Global.HEADS_UP_NOTIFICATIONS_ENABLED`): one over a row makes the agent
    // refuse the row's tap.
    setting("global", "heads_up_notifications_enabled", "0")?;
    // Taps land where the agent saw an element, not on one still moving in.
    for scale in [
        "window_animation_scale",
        "transition_animation_scale",
        "animator_duration_scale",
    ] {
        setting("global", scale, "0")?;
    }
    Logcat::follow()
}

/// No on-screen keyboard beside the AVD's hardware one (the emulator's default shows it): taps are
/// touches, which the keyboard's window takes (the profile's Next), and text goes in through the
/// agent. The keyboard app stays bound, which Compose's fields need. Set once the device is set
/// up: set right after boot, it reads 1 again by registration. Agent.kt `tap` also refuses a tap
/// a window in front would take.
fn no_soft_keyboard() -> Result<()> {
    setting("secure", "show_ime_with_hard_keyboard", "0")
}

/// Sets `name` in `namespace` (`settings put`) to `value`, and reads it back.
fn setting(namespace: &str, name: &str, value: &str) -> Result<()> {
    adb(&["shell", "settings", "put", namespace, name, value])?;
    let now = adb(&["shell", "settings", "get", namespace, name])?;
    if now.trim() == value {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "{namespace} {name} reads {now:?}, not {value}"
        )))
    }
}

/// Hands each line of `stream` to `each`, on a thread of its own, until it ends; `what` names it
/// in an error.
fn each_line(
    stream: impl Read + Send + 'static,
    what: &'static str,
    mut each: impl FnMut(String) + Send + 'static,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            match line {
                Ok(line) => each(line),
                Err(error) => {
                    eprintln!("android: reading {what}: {error}");
                    return;
                }
            }
        }
    });
}

/// Passes `stream`'s lines on to our stdout behind `prefix`; the receiver hears once when one
/// contains `needle`, and is closed if the stream ends first.
fn announced(
    stream: impl Read + Send + 'static,
    what: &'static str,
    prefix: &'static str,
    needle: &'static str,
) -> mpsc::Receiver<()> {
    let (heard, hear) = mpsc::channel();
    let mut listener = Some(heard);
    each_line(stream, what, move |line| {
        println!("{prefix}{line}");
        if line.contains(needle)
            && let Some(heard) = listener.take()
            && let Err(mpsc::SendError(())) = heard.send(())
        {
            // Nobody waits any more: the lines go on to our stdout only.
        }
    });
    hear
}

/// Installs the APK; the launcher activity of the app it installed, `package/activity`.
fn install(apk: &Path) -> Result<String> {
    let path = apk.display().to_string();
    adb(&["install", "-r", "-g", &path])?;
    let packages = adb(&["shell", "pm", "list", "packages", PACKAGE_PREFIX])?;
    let local: Vec<&str> = packages
        .split_whitespace()
        .filter_map(|line| line.strip_prefix("package:"))
        .filter(|package| package.ends_with(".local"))
        .collect();
    let [package] = local.as_slice() else {
        return Err(Error::Failed(format!("not one local package: {local:?}")));
    };
    let resolved = adb(&[
        "shell",
        "cmd",
        "package",
        "resolve-activity",
        "--brief",
        "-c",
        "android.intent.category.LAUNCHER",
        package,
    ])?;
    resolved
        .split_whitespace()
        .last()
        .map(str::to_owned)
        .ok_or_else(|| Error::Failed(format!("no launcher activity for {package}")))
}

/// The logcat lines a wait is for, since the driver started: Signal-Android's own word on what it
/// has done, as SignalCI's events are Desktop's.
struct Logcat {
    lines: Mutex<Vec<String>>,
    grew: Condvar,
}

static LOGCAT_LINES: Logcat = Logcat {
    lines: Mutex::new(Vec::new()),
    grew: Condvar::new(),
};

impl Logcat {
    /// Follows `adb logcat` to [`LOGCAT`] for the box's logs, and into [`LOGCAT_LINES`] the lines
    /// a wait is for ([`STORAGE_SYNCED`]): the rest, the most of it, isn't kept in memory.
    fn follow() -> Result<()> {
        let mut logcat = Command::new("adb")
            .args(["logcat", "-v", "time"])
            .stdout(Stdio::piped())
            .spawn()
            .context("adb logcat")?;
        let stdout = logcat
            .stdout
            .take()
            .ok_or_else(|| Error::Failed("adb logcat's stdout isn't piped".to_owned()))?;
        // A line at a time, as it is read: whole lines in the file for the box's logs.
        let mut file = LineWriter::new(std::fs::File::create(LOGCAT).context(LOGCAT)?);
        each_line(stdout, "logcat", move |line| {
            if let Err(error) = writeln!(file, "{line}") {
                eprintln!("android: writing {LOGCAT}: {error}");
            }
            if !line.contains(STORAGE_SYNCED) {
                return;
            }
            match LOGCAT_LINES.lines.lock() {
                Ok(mut lines) => lines.push(line),
                // A wait reports it.
                Err(_poisoned) => eprintln!("android: logcat's lines are poisoned"),
            }
            LOGCAT_LINES.grew.notify_all();
        });
        Ok(())
    }

    fn lines(&self) -> Result<MutexGuard<'_, Vec<String>>> {
        self.lines
            .lock()
            .map_err(|_poisoned| Error::Failed("logcat's follower panicked".to_owned()))
    }

    fn mark(&self) -> Result<u64> {
        let count = self.lines()?.len();
        u64::try_from(count).describe("logcat's line count")
    }

    /// Waits, woken by each new line, until a line after `after` contains `needle`.
    fn await_line(&self, after: u64, needle: &str, patience: Duration) -> Result<()> {
        // Each wake looks at the lines that came since the last one only.
        let mut scanned = usize::try_from(after).describe("a logcat mark")?;
        let (_lines, waited) = self
            .grew
            .wait_timeout_while(self.lines()?, patience, |lines| {
                let found = lines
                    .get(scanned..)
                    .is_some_and(|new| new.iter().any(|line| line.contains(needle)));
                scanned = scanned.max(lines.len());
                !found
            })
            .map_err(|_poisoned| Error::Failed("logcat's lines are poisoned".to_owned()))?;
        if waited.timed_out() {
            Err(Error::Failed(format!(
                "no {needle:?} in logcat within {} ms",
                patience.as_millis()
            )))
        } else {
            Ok(())
        }
    }
}

/// The on-device agent (testbox/android/agent): it holds UiAutomation, which Android gives one
/// connection at a time, so the screen is read and waited on only through it, not `uiautomator`.
struct Agent {
    instrumentation: Child,
    calls: BufReader<UnixStream>,
    next_id: u32,
}

impl Agent {
    /// Installs and starts it, and connects once it says it listens.
    fn start() -> Result<Self> {
        adb(&["install", "-r", AGENT_APK])?;
        let mut instrumentation = Command::new("adb")
            .args([
                "shell",
                "am",
                "instrument",
                "-w",
                "-r",
                AGENT_INSTRUMENTATION,
            ])
            .stdout(Stdio::piped())
            .spawn()
            .context("the agent's instrumentation")?;
        let stdout = instrumentation
            .stdout
            .take()
            .ok_or_else(|| Error::Failed("the instrumentation's stdout isn't piped".to_owned()))?;
        announced(stdout, "the agent's output", "agent: ", AGENT_ABSTRACT)
            .recv()
            .map_err(|_closed| {
                Error::Failed("the agent's instrumentation ended before it listened".to_owned())
            })?;
        remove_if_present(Path::new(AGENT_SOCKET))?;
        adb(&[
            "forward",
            &format!("localfilesystem:{AGENT_SOCKET}"),
            &format!("localabstract:{AGENT_ABSTRACT}"),
        ])?;
        let stream = UnixStream::connect(AGENT_SOCKET).context(AGENT_SOCKET)?;
        Ok(Self {
            instrumentation,
            calls: BufReader::new(stream),
            next_id: 1,
        })
    }

    /// A wait's answer: the screen it found.
    fn look(&mut self, call: agent::Call) -> Result<agent::Found> {
        match self.call(call)? {
            agent::Outcome::Done(found) => Ok(found),
            other => Err(Error::Failed(format!(
                "the agent answered a wait with {other:?}"
            ))),
        }
    }

    /// An action's answer: done.
    fn act(&mut self, call: agent::Call) -> Result<()> {
        match self.call(call)? {
            agent::Outcome::Acted => Ok(()),
            other => Err(Error::Failed(format!(
                "the agent answered an action with {other:?}"
            ))),
        }
    }

    /// Taps (or long-presses) `node` of the last answer's screen.
    fn click(&mut self, node: &agent::Node, long: bool) -> Result<()> {
        self.act(agent::Call::Click {
            handle: node.handle,
            long,
        })
    }

    fn call(&mut self, call: agent::Call) -> Result<agent::Outcome> {
        let id = self.next_id;
        self.next_id += 1;
        let line =
            serde_json::to_string(&agent::Request { id, call }).context("a call to the agent")?;
        writeln!(self.calls.get_mut(), "{line}").context("calling the agent")?;
        let mut reply = String::new();
        let read = self
            .calls
            .read_line(&mut reply)
            .context("the agent's reply")?;
        if read == 0 {
            return Err(Error::Failed(format!(
                "the agent hung up ({})",
                match self.instrumentation.try_wait() {
                    Ok(Some(status)) => format!("its instrumentation exited: {status}"),
                    Ok(None) => "its instrumentation runs".to_owned(),
                    Err(error) => format!("its instrumentation: {error}"),
                }
            )));
        }
        let reply: agent::Reply =
            serde_json::from_str(&reply).with_context(|| format!("the agent's reply {reply}"))?;
        match reply.outcome {
            agent::Outcome::Failed(error) => Err(Error::Failed(error)),
            _ if reply.id != id => Err(Error::Failed(format!(
                "the agent answered call {}, not {id}",
                reply.id
            ))),
            outcome => Ok(outcome),
        }
    }
}

/// A wait's bound as the agent takes it, in milliseconds (u32: 49 days).
fn timeout_ms(patience: Duration) -> u32 {
    u32::try_from(patience.as_millis()).unwrap_or(u32::MAX)
}

struct Driver {
    /// The app's launcher activity, `package/activity`.
    launcher: String,
    agent: Agent,
}

impl Driver {
    fn answer(&mut self, call: Android) -> Result<Answered> {
        let done = match call {
            Android::Register { number, name } => self.register(&number, &name),
            Android::NoteToSelf { text } => self.note_to_self(&text),
            Android::LinkDevice { url, transfer } => self.link_device(&url, transfer),
            Android::OpenConversation { text } => {
                self.launch()?;
                self.tap(&text, PATIENCE)
            }
            Android::Send { text } => self.send(&text),
            Android::WaitFor { wanted, timeout_ms } => self
                .first_shown(vec![wanted], Duration::from_millis(timeout_ms))
                .map(drop),
            Android::ChatMenu { text, item } => self.chat_menu(&text, item),
            Android::ChatMenuOffers { text, items } => self.chat_menu_offers(&text, &items),
            Android::AddSuggestedFolder { name } => self.add_suggested_folder(&name),
            Android::GroupMember {
                group,
                member,
                change,
            } => self.group_member(&group, &member, change),
            Android::GroupLink { group, approval } => self.group_link(&group, approval),
            Android::ApproveRequest { group, member } => self.approve_request(&group, &member),
            Android::LogMark => return LOGCAT_LINES.mark().map(Answered::Mark),
            Android::AwaitStorageSync { after, timeout_ms } => {
                LOGCAT_LINES.await_line(after, STORAGE_SYNCED, Duration::from_millis(timeout_ms))
            }
        };
        done.map(|()| Answered::Done)
    }

    /// Opens the menu of the chat list's row showing `text` (a long click on it); the menu's own
    /// window, by its last item, Delete, which shows with all of them (ConversationListFragment.java
    /// `onConversationLongClick`, strings.xml `ConversationListFragment_delete`): its items, not
    /// their namesakes behind it.
    fn open_chat_menu(&mut self, text: &str) -> Result<Vec<agent::Node>> {
        self.launch()?;
        let row = self.element(text, PATIENCE)?.node;
        self.agent.click(&row, true)?;
        let shown = self.element("Delete", PATIENCE)?;
        let window = shown.node.window;
        Ok(shown
            .screen
            .into_iter()
            .filter(|node| node.window == window)
            .collect())
    }

    fn chat_menu(&mut self, text: &str, item: ChatMenuItem) -> Result<()> {
        let menu = self.open_chat_menu(text)?;
        let entry = menu
            .iter()
            .find(|node| node.labelled(item.label()))
            .ok_or_else(|| Error::Failed(format!("the menu of {text:?} has no {item:?}")))?;
        self.tap_at(entry)?;
        if item == ChatMenuItem::Mute {
            // MuteDialog.kt's last option (`arrays__always`).
            self.tap("Always", PATIENCE)?;
        }
        Ok(())
    }

    /// Opens the row's menu once and reads it: a menu shows the chat as it was when opened, so
    /// the caller waits for what it expects to have happened first ([`Android::AwaitStorageSync`]).
    fn chat_menu_offers(&mut self, text: &str, items: &[ChatMenuItem]) -> Result<()> {
        let menu = self.open_chat_menu(text)?;
        adb(&["shell", "input", "keyevent", "KEYCODE_BACK"])?;
        // Gone before whatever comes next: relaunching on a cleared task while the menu closes
        // crashes Signal-Android ("ConversationListFragment … is not currently in the
        // FragmentManager", from Compose disposing the menu's content).
        self.agent.act(agent::Call::WaitGone {
            wanted: vec![agent::Wanted::label("Delete")],
            timeout_ms: timeout_ms(PATIENCE),
        })?;
        let offered: Vec<ChatMenuItem> = ChatMenuItem::VARIANTS
            .iter()
            .copied()
            .filter(|item| menu.iter().any(|node| node.labelled(item.label())))
            .collect();
        let missing: Vec<&ChatMenuItem> = items
            .iter()
            .filter(|item| !offered.contains(item))
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(Error::Failed(format!(
                "the menu of {text:?} offers no {missing:?}, but {offered:?}"
            )))
        }
    }

    /// Settings, Chats, "Add a chat folder", then the `Add` in the suggestion's row. Whether the
    /// folder came is for its storage sync to show (the xtask waits for it in b33p): the app's
    /// own word, a snackbar (`ChatFoldersFragment__folder_added`), is gone in a moment.
    fn add_suggested_folder(&mut self, name: &str) -> Result<()> {
        self.settings()?;
        self.tap("Chats", PATIENCE)?;
        self.tap("Add a chat folder", PATIENCE)?;
        // Its explanation comes first, once (ChatFoldersEducationSheet.kt, strings.xml
        // `ChatFoldersEducationSheet__continue`).
        let mut shown = self.first_shown(
            vec![agent::Wanted::label("Continue"), agent::Wanted::label(name)],
            PATIENCE,
        )?;
        if shown.which == 0 {
            self.tap_at(&shown.node)?;
            shown = self.element(name, PATIENCE)?;
        }
        let row = shown.node.y;
        let add = shown
            .screen
            .iter()
            .filter(|node| node.labelled("Add"))
            .min_by_key(|node| node.y.abs_diff(row))
            .ok_or_else(|| Error::Failed(format!("no Add for the folder {name:?}")))?;
        self.tap_at(add)
    }

    /// The group's settings, the member's sheet, its item, then the confirmation.
    fn group_member(&mut self, group: &str, member: &str, change: MemberChange) -> Result<()> {
        self.group_settings_row(group, member)?;
        self.tap(change.item(), PATIENCE)?;
        let (message, button) = change.confirmation();
        self.confirm(message, button)
    }

    /// The invite link's page (ShareableGroupLinkFragment): its switch, then, once the link is on,
    /// the approval's, each by its row's label, whose tap toggles it.
    fn group_link(&mut self, group: &str, approval: bool) -> Result<()> {
        self.group_settings_row(group, "Group link")?;
        // The page, by the approval's row (strings.xml
        // `ShareableGroupLinkDialogFragment__require_admin_approval`), there while the link is off
        // too, disabled; the link's row is the lower "Group link" (the page's title is the other).
        let shown = self.element("Require admin approval", PATIENCE)?;
        let row = shown
            .screen
            .iter()
            .filter(|node| node.labelled("Group link"))
            .max_by_key(|node| node.y)
            .ok_or("no \"Group link\" row")?;
        self.tap_at(row)?;
        // On once the app has turned the link on with the server: a tap on the approval before
        // that was lost, and the link stayed open to anyone.
        self.switch_on("Group link")?;
        if approval {
            self.tap("Require admin approval", PATIENCE)?;
            self.switch_on("Require admin approval")?;
        }
        Ok(())
    }

    /// Waits until the switch of the lowest row labelled `row` is on. The rows are Compose's
    /// (core/ui Rows.ToggleRow): its Switch is a node of its own, `android.view.View` with the
    /// role of a switch, checked when on, level with the row's text; while the row's change is
    /// under way, a progress indicator stands in its place. Each look waits for the screen to
    /// settle.
    fn switch_on(&mut self, row: &str) -> Result<()> {
        /// How far a row's switch may sit from its text, in pixels.
        const LEVEL: u32 = 100;
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            let shown = self.element(row, PATIENCE)?;
            let label = shown
                .screen
                .iter()
                .filter(|node| node.labelled(row))
                .max_by_key(|node| node.y)
                .ok_or_else(|| Error::Failed(format!("no {row:?} row")))?;
            let level: Vec<&agent::Node> = shown
                .screen
                .iter()
                .filter(|node| node.y.abs_diff(label.y) <= LEVEL)
                .collect();
            if level.iter().any(|node| node.checked) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                let nodes: Vec<String> = level
                    .iter()
                    .map(|node| {
                        format!(
                            "{:?}/{:?} {} checked {} enabled {}",
                            node.text,
                            node.content_desc,
                            node.class_name,
                            node.checked,
                            node.enabled
                        )
                    })
                    .collect();
                return Err(Error::Failed(format!(
                    "the switch of {row:?} isn't on; level with it: {}",
                    nodes.join("; ")
                )));
            }
        }
    }

    /// The request among "Requests & invites" (PendingMembersActivity, strings.xml
    /// `PendingMembersActivity_requests`): its Approve beside `member`'s name
    /// (`GroupRecipientListItem_approve_description`), then the confirmation's Add
    /// (`RequestConfirmationDialog_add_s_to_the_group`, `RequestConfirmationDialog_add`).
    fn approve_request(&mut self, group: &str, member: &str) -> Result<()> {
        self.group_settings_row(group, "Requests & invites")?;
        let mut shown = self.first_shown(
            vec![
                agent::Wanted::label(member),
                agent::Wanted::label("Requests"),
            ],
            PATIENCE,
        )?;
        if shown.which == 1 {
            self.tap_at(&shown.node)?;
            shown = self.element(member, PATIENCE)?;
        }
        let row = shown.node.y;
        let approve = shown
            .screen
            .iter()
            .filter(|node| node.labelled("Approve"))
            .min_by_key(|node| node.y.abs_diff(row))
            .ok_or_else(|| Error::Failed(format!("no Approve for {member:?}")))?;
        self.tap_at(approve)?;
        self.confirm("to the group?", "Add")
    }

    /// The group `group` names, opened at its settings, scrolled to `row`, which is tapped.
    fn group_settings_row(&mut self, group: &str, row: &str) -> Result<()> {
        self.launch()?;
        self.tap(group, PATIENCE)?;
        // The conversation, by its composer (the chat list has no field), whose title opens its
        // settings: the topmost of the name's namesakes (the conversation's header shows it too).
        let shown = self.first_shown(vec![agent::Wanted::Editable], PATIENCE)?;
        let title = shown
            .screen
            .iter()
            .filter(|node| node.labelled(group))
            .min_by_key(|node| node.y)
            .ok_or_else(|| Error::Failed(format!("no title {group:?}")))?;
        self.tap_at(title)?;
        // The settings, by their first row (strings.xml
        // `ConversationSettingsFragment__disappearing_messages`), before a member is looked for.
        self.element("Disappearing messages", PATIENCE)?;
        let found = self.agent.look(agent::Call::ScrollTo {
            label: row.to_owned(),
            timeout_ms: timeout_ms(PATIENCE),
        })?;
        self.tap_at(&found.node)
    }

    /// The button `button` of the dialog whose message has `message` in it: in the dialog's
    /// window, not a namesake behind it ("Make admin" is both).
    fn confirm(&mut self, message: &str, button: &str) -> Result<()> {
        let dialog = self.first_shown(vec![agent::Wanted::containing(message)], PATIENCE)?;
        let confirm = dialog
            .screen
            .iter()
            .find(|node| node.window == dialog.node.window && node.labelled(button))
            .ok_or_else(|| Error::Failed(format!("no {button:?} by {message:?}")))?;
        self.tap_at(confirm)
    }

    /// The app from its launcher entry, on a fresh task: its main screen, not whatever was last
    /// open (FLAG_ACTIVITY_NEW_TASK | FLAG_ACTIVITY_CLEAR_TASK).
    fn launch(&self) -> Result<()> {
        adb(&[
            "shell",
            "am",
            "start",
            "-W",
            "-f",
            "0x10008000",
            "-n",
            &self.launcher,
        ])
        .map(drop)
    }

    /// The first of `wanted` to show, on the settled screen; a failure with the last screen after
    /// `patience`.
    fn first_shown(
        &mut self,
        wanted: Vec<agent::Wanted>,
        patience: Duration,
    ) -> Result<agent::Found> {
        self.agent.look(agent::Call::WaitFor {
            wanted,
            timeout_ms: timeout_ms(patience),
        })
    }

    fn element(&mut self, text: &str, patience: Duration) -> Result<agent::Found> {
        self.first_shown(vec![agent::Wanted::label(text)], patience)
    }

    /// Clicks `node` of the last answer's screen.
    fn tap_at(&mut self, node: &agent::Node) -> Result<()> {
        self.agent.click(node, false)
    }

    /// Taps the control labelled `text`, once it is enabled.
    fn tap(&mut self, text: &str, patience: Duration) -> Result<()> {
        let found = self.first_shown(vec![agent::Wanted::control(text)], patience)?;
        self.tap_at(&found.node)
    }

    /// Sets the text of the field with input focus, once one has it, and waits until it reads as
    /// `echo` says: through accessibility, not keys, which go astray while the keyboard is still
    /// binding to a field just focused (a search for "ANR" arrives as "R", a phone number not at
    /// all).
    fn type_text(&mut self, text: &str, echo: agent::Echo) -> Result<()> {
        self.agent.act(agent::Call::SetText {
            text: text.to_owned(),
            echo,
            timeout_ms: timeout_ms(PATIENCE),
        })
    }

    /// Gives `field` focus, as a tap does, and sets its text.
    fn fill(&mut self, field: &agent::Node, text: &str, echo: agent::Echo) -> Result<()> {
        self.tap_at(field)?;
        self.type_text(text, echo)
    }

    /// Registers `number` (+1 only: the default country) through the app's own screens, with the
    /// box's captcha page and any code, the PIN disabled (no SVR in the box), and `name` as the
    /// profile's; then turns Signal's ANR crash off (`disable_anr_crashes`).
    fn register(&mut self, number: &str, name: &str) -> Result<()> {
        let national = number.strip_prefix("+1").ok_or_else(|| {
            Error::Failed(format!("{number}: only +1 numbers, the default country"))
        })?;
        self.launch()?;
        self.tap("Continue", PATIENCE)?;
        self.element("Phone number", PATIENCE)?;
        let field = self
            .first_shown(vec![agent::Wanted::Editable], PATIENCE)?
            .node;
        self.fill(&field, national, agent::Echo::Digits(national.to_owned()))?;
        self.tap("Next", PATIENCE)?;
        self.tap("OK", PATIENCE)?;
        self.element("Verification code", SLOW)?;
        self.type_text(testbox_tools::VERIFICATION_CODE, agent::Echo::Anything)?;
        self.tap("Open menu", SLOW)?;
        self.tap("Disable PIN", PATIENCE)?;
        // The warning's own button has the menu item's label.
        self.element("Warning", PATIENCE)?;
        self.tap("Disable PIN", PATIENCE)?;
        self.tap("First name (required)", SLOW)?;
        self.type_text(name, agent::Echo::Exact(name.to_owned()))?;
        self.tap("Next", PATIENCE)?;
        self.element("Chats", SLOW)?;
        self.disable_anr_crashes()
    }

    /// A debug build is an internal user (RemoteConfig.kt `internalUser`, Environment.isInternal),
    /// and Signal crashes an internal user's app when the main thread stalls for 5 s
    /// (AnrDetector.kt `SignalAnrException`). Opening the camera does on the emulator:
    /// CameraScreen.kt waits for CameraX on the main thread.
    fn disable_anr_crashes(&mut self) -> Result<()> {
        self.settings()?;
        let preferences = self.agent.look(agent::Call::ScrollTo {
            label: "Internal Preferences".to_owned(),
            timeout_ms: timeout_ms(SLOW),
        })?;
        self.tap_at(&preferences.node)?;
        self.tap("Search", PATIENCE)?;
        self.type_text("ANR", agent::Echo::Exact("ANR".to_owned()))?;
        let shown = self.element("Enable ANR-induced crashing", PATIENCE)?;
        let switches: Vec<&agent::Node> = shown
            .screen
            .iter()
            .filter(|node| node.class_name == "android.widget.Switch")
            .collect();
        let [switch] = switches.as_slice() else {
            return Err(Error::Failed(format!(
                "not one switch for ANR crashes: {}",
                switches.len()
            )));
        };
        if switch.checked {
            self.tap_at(switch)?;
        }
        Ok(())
    }

    /// Links the device showing `url` as a QR: the QR on the camera scene's wall poster, scanned
    /// by the app's own scanner (not the `sgnl://linkdevice` deep link, which opens the linked
    /// devices without the link, `DeviceProvisioningActivity.java:36`); with the message history
    /// if `transfer`, which goes through the attachment CDN (CDN3).
    fn link_device(&mut self, url: &str, transfer: bool) -> Result<()> {
        show_qr(url)?;
        self.settings()?;
        self.tap("Linked devices", PATIENCE)?;
        self.tap("Link a new device", PATIENCE)?;
        let choice = if transfer {
            "Transfer message history"
        } else {
            "Don't transfer"
        };
        // The camera's explanation comes first, once.
        let shown = self.first_shown(
            vec![agent::Wanted::label("Okay"), agent::Wanted::label(choice)],
            SLOW,
        )?;
        self.tap_at(&shown.node)?;
        if shown.which == 0 {
            self.tap(choice, SLOW)?;
        }
        self.launch()
    }

    /// Sends `text` to Note to Self, from the new-chat list.
    fn note_to_self(&mut self, text: &str) -> Result<()> {
        self.launch()?;
        self.tap("New chat", PATIENCE)?;
        self.tap("Note to Self", PATIENCE)?;
        self.send(text)
    }

    /// Sends `text` in the open conversation, from its composer: the screen's last field.
    fn send(&mut self, text: &str) -> Result<()> {
        let shown = self.first_shown(vec![agent::Wanted::Editable], PATIENCE)?;
        let field = shown
            .screen
            .into_iter()
            .rfind(|node| node.editable)
            .ok_or_else(|| Error::Failed("no message field".to_owned()))?;
        self.fill(&field, text, agent::Echo::Exact(text.to_owned()))?;
        self.tap("Send message", PATIENCE)
    }

    fn settings(&mut self) -> Result<()> {
        self.launch()?;
        // Without Play Services the app may ask first.
        let shown = self.first_shown(
            vec![
                agent::Wanted::label("Optimize for missing Play Services"),
                agent::Wanted::label("More options"),
            ],
            PATIENCE,
        )?;
        if shown.which == 0 {
            self.tap("Cancel", PATIENCE)?;
            self.tap("More options", PATIENCE)?;
        } else {
            self.tap_at(&shown.node)?;
        }
        self.tap("Settings", PATIENCE)
    }
}

/// What a call answers with: nothing, or a logcat mark.
#[derive(Serialize)]
#[serde(untagged)]
enum Answered {
    Done,
    Mark(u64),
}

fn serve(driver: &mut Driver, socket: &Path) -> Result<()> {
    remove_if_present(socket)?;
    let listener = UnixListener::bind(socket).context(socket.display().to_string())?;
    // The owner's only, as b33p's harness socket is (b33p-slint/src/agent.rs): the socket's
    // directory is on the host.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .context(socket.display().to_string())?;
    println!("{} {}", drivers::LISTENING, socket.display());
    for stream in listener.incoming() {
        let stream = stream.context("accepting a connection")?;
        // A connection that fails ends; the driver serves the next.
        if let Err(error) = answer_calls(driver, stream) {
            eprintln!("android: {error}");
        }
    }
    Ok(())
}

fn answer_calls(driver: &mut Driver, stream: UnixStream) -> Result<()> {
    let mut writer = stream
        .try_clone()
        .context("a second handle to the connection")?;
    for line in BufReader::new(stream).lines() {
        let line = line.context("reading a call")?;
        let answer = match serde_json::from_str::<Numbered<Android>>(&line) {
            Ok(Numbered { id, call }) => match driver.answer(call) {
                Ok(result) => Answer::Done { id, result },
                Err(error) => Answer::Failed {
                    id: Some(id),
                    error: error.to_string(),
                },
            },
            Err(error) => Answer::Failed {
                id: serde_json::from_str::<drivers::Id>(&line)
                    .ok()
                    .and_then(|call| call.id),
                error: format!("not a call: {error}: {line}"),
            },
        };
        let reply = serde_json::to_string(&answer).context("an answer")?;
        writeln!(writer, "{reply}").context("answering")?;
    }
    Ok(())
}
