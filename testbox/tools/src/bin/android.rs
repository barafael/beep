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
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Condvar, Mutex, MutexGuard, mpsc};
use std::time::Duration;

use serde::Serialize;
use strum::VariantArray;
use testbox_tools::drivers::{self, Android, Answer, ChatMenuItem, MemberChange, Numbered};
use testbox_tools::keys::{self, DesktopConfig, KeyFile};
use testbox_tools::process::{CommandError, describe, output};
use testbox_tools::{EnvError, FileError, env_var, read, remove_if_present, write};
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
/// How long a look at the screen takes to see what it settled on.
const GLANCE: Duration = Duration::from_secs(2);
/// The on-device agent (testbox/android/agent), its instrumentation, and the socket it listens on,
/// which `am instrument -r` prints once it does (Agent.kt `sendStatus`).
const AGENT_APK: &str = "/opt/testbox/agent.apk";
const AGENT_INSTRUMENTATION: &str = "org.b33p.testbox.agent/.Agent";
const AGENT_ABSTRACT: &str = "b33p-testbox-agent";
/// adb's end of the agent's socket, forwarded to it.
const AGENT_SOCKET: &str = "/tmp/agent.sock";
/// What StorageSyncJob.kt logs once it has taken in a newer manifest.
const STORAGE_SYNCED: &str = "[Remote Sync] Saved new manifest";

const USAGE: &str =
    "android [config | verification <metadata> <group> <version> <out> | show-qr <url>]";

/// What a command of the tool, or a call of the driver's, reports: the errors below, or the
/// library's.
type Failure = Box<dyn std::error::Error + Send + Sync>;

/// The tool was given arguments it doesn't take.
#[derive(Debug, thiserror::Error)]
#[error("usage: {USAGE}")]
struct UsageError;

/// What keeps an image stage's output (`config`, `verification`) from being written.
#[derive(Debug, thiserror::Error)]
enum StageError {
    #[error("creating {}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What keeps the local flavor's values from being made of the box's keys, or the image's APK
/// from serving them.
#[derive(Debug, thiserror::Error)]
enum LocalConfigError {
    #[error("reading the box's keys")]
    Keys(#[source] FileError),
    #[error("parsing {}", path.display())]
    Desktop {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{} has no certificate", path.display())]
    NoCertificate { path: PathBuf },
    #[error("{} has no trust root", path.display())]
    NoTrustRoot { path: PathBuf },
    #[error("serializing the local configuration")]
    Serialize(#[source] serde_json::Error),
    #[error(
        "the image's APK was built for other keys than the box's ({}): build the android image \
         after the server image, which holds the keys",
        path.display()
    )]
    OtherKeys { path: PathBuf },
}

/// A program whose output the driver follows, which didn't start ([`spawn_piped`]).
#[derive(Debug, thiserror::Error)]
enum SpawnError {
    #[error("starting {command}")]
    Start {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{command}'s stdout isn't piped")]
    NoStdout { command: String },
}

/// What keeps the emulator from booting, or Android on it from being set up for the box.
#[derive(Debug, thiserror::Error)]
enum BootError {
    #[error("listing the AVD {}", path.display())]
    ListAvd {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("removing the AVD's lock {}", path.display())]
    RemoveLock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Its files, or the environment that says where they are and the display's size.
    #[error("configuring the AVD")]
    AvdFile(#[source] FileError),
    #[error("configuring the AVD")]
    AvdEnvironment(#[source] EnvError),
    #[error("configuring the AVD {}", path.display())]
    AvdConfig {
        path: PathBuf,
        #[source]
        source: avd::ConfigError,
    },
    #[error("starting adb's server")]
    AdbServer(#[source] CommandError),
    #[error("booting the emulator")]
    Emulator(#[source] SpawnError),
    #[error("the emulator exited while booting: {status}")]
    EmulatorExited { status: ExitStatus },
    #[error("the emulator's output ended while booting")]
    EmulatorOutputEnded,
    #[error("looking at the emulator, whose output ended while booting")]
    EmulatorGone(#[source] std::io::Error),
    #[error("asking Android whether it booted")]
    BootCompleted(#[source] CommandError),
    #[error("the emulator says it booted, Android's sys.boot_completed {boot_completed:?}")]
    NotBooted { boot_completed: String },
    #[error("getting adb root")]
    Root(#[source] CommandError),
    #[error("raising Android's ANR timeouts")]
    Timeouts(#[source] CommandError),
    #[error("ro.hw_timeout_multiplier is {seen:?}, not {wanted}")]
    Multiplier { seen: String, wanted: u32 },
    #[error("following Android's event log")]
    EventLog(#[source] SpawnError),
    #[error("the event log ended before the framework started")]
    EventLogEnded,
    #[error("stopping {command}")]
    Stop {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("forwarding the device's port {port} to the pod's")]
    Reverse {
        port: u16,
        #[source]
        source: CommandError,
    },
    #[error("setting {namespace} {name}")]
    Setting {
        namespace: String,
        name: String,
        #[source]
        source: CommandError,
    },
    #[error("{namespace} {name} reads {seen:?}, not {wanted}")]
    SettingReads {
        namespace: String,
        name: String,
        seen: String,
        wanted: String,
    },
    #[error("turning the gesture navigation on")]
    GestureNavigation(#[source] CommandError),
    #[error("following logcat")]
    Logcat(#[source] SpawnError),
    #[error("creating {}", path.display())]
    CreateLog {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What keeps the APK from being installed, or its launcher activity from being found.
#[derive(Debug, thiserror::Error)]
enum InstallError {
    #[error("installing {}", apk.display())]
    Install {
        apk: PathBuf,
        #[source]
        source: CommandError,
    },
    #[error("listing the installed packages")]
    Packages(#[source] CommandError),
    #[error("not one local package: {packages:?}")]
    NotOneLocalPackage { packages: Vec<String> },
    #[error("resolving {package}'s launcher activity")]
    Launcher {
        package: String,
        #[source]
        source: CommandError,
    },
    #[error("no launcher activity for {package}")]
    NoLauncher { package: String },
}

/// What keeps the on-device agent from being installed, started or reached.
#[derive(Debug, thiserror::Error)]
enum AgentStartError {
    #[error("installing {}", apk.display())]
    Install {
        apk: PathBuf,
        #[source]
        source: CommandError,
    },
    #[error("starting the agent's instrumentation")]
    Instrumentation(#[source] SpawnError),
    #[error("the agent's instrumentation ended before it listened")]
    NotListening,
    #[error("clearing adb's end of the agent's socket")]
    Socket(#[source] FileError),
    #[error("forwarding the agent's socket")]
    Forward(#[source] CommandError),
    #[error("connecting to {}", path.display())]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A call to the on-device agent that failed, the call with it.
#[derive(Debug, thiserror::Error)]
enum AgentError {
    #[error("serializing {call:?}")]
    Serialize {
        call: agent::Call,
        #[source]
        source: serde_json::Error,
    },
    #[error("sending {call:?}")]
    Send {
        call: agent::Call,
        #[source]
        source: std::io::Error,
    },
    #[error("reading the answer to {call:?}")]
    Receive {
        call: agent::Call,
        #[source]
        source: std::io::Error,
    },
    #[error("the agent hung up on {call:?} ({})", instrumentation(*status))]
    HungUp {
        call: agent::Call,
        /// The instrumentation's exit, if it exited.
        status: Option<ExitStatus>,
    },
    #[error("the agent hung up on {call:?}; looking at its instrumentation")]
    Instrumentation {
        call: agent::Call,
        #[source]
        source: std::io::Error,
    },
    #[error("the answer to {call:?}, {reply}")]
    Reply {
        call: agent::Call,
        reply: String,
        #[source]
        source: serde_json::Error,
    },
    /// The agent's own word on why (Agent.kt).
    #[error("{call:?}: {message}")]
    Failed { call: agent::Call, message: String },
    #[error("the agent answered call {answered}, not {id} ({call:?})")]
    OtherCall {
        call: agent::Call,
        id: u32,
        answered: u32,
    },
    /// Boxed: an outcome may carry a whole screen.
    #[error("the agent answered {call:?} with {outcome:?}")]
    Unexpected {
        call: agent::Call,
        outcome: Box<agent::Outcome>,
    },
}

impl From<AgentError> for UiError {
    fn from(error: AgentError) -> Self {
        Self::Agent(Box::new(error))
    }
}

/// The instrumentation, as [`AgentError::HungUp`] says it.
fn instrumentation(status: Option<ExitStatus>) -> String {
    match status {
        Some(status) => format!("its instrumentation exited: {status}"),
        None => "its instrumentation runs".to_owned(),
    }
}

/// What keeps a step on the app's screens from being done, or shows it wasn't.
#[derive(Debug, thiserror::Error)]
enum UiError {
    /// Boxed: a call carries the screen it waited on, which would make every `UiError` large.
    #[error("asking the on-device agent")]
    Agent(#[source] Box<AgentError>),
    #[error("pressing {key}")]
    Key {
        key: &'static str,
        #[source]
        source: CommandError,
    },
    #[error("tapping ({x}, {y})")]
    TapAt {
        x: u32,
        y: u32,
        #[source]
        source: CommandError,
    },
    #[error("launching {activity}")]
    Launch {
        activity: String,
        #[source]
        source: CommandError,
    },
    #[error("showing the QR of {url}")]
    ShowQr {
        url: String,
        #[source]
        source: CommandError,
    },
    #[error("reading the screen's size")]
    ScreenSize(#[source] CommandError),
    #[error("wm size answered {answer:?}")]
    ScreenSizeAnswer { answer: String },
    #[error("running adb exec-out screencap")]
    Screencap(#[source] std::io::Error),
    #[error("adb exec-out screencap: {status}, {length} bytes")]
    ScreencapFailed { status: ExitStatus, length: usize },
    #[error("screencap format {format}, not RGBA_8888")]
    ScreencapFormat { format: u32 },
    /// The screenshot is kept at `kept`.
    #[error("no reaction bar with its heart after a long press on {text:?} ({kept})")]
    NoReactionBar { text: String, kept: String },
    #[error("{seconds} s isn't one of the app's presets")]
    Preset { seconds: u32 },
    #[error("not back in the conversation")]
    NotBackInConversation,
    #[error("the menu of {text:?} has no {item:?}")]
    MenuItem { text: String, item: ChatMenuItem },
    #[error("the menu of {text:?} offers no {missing:?}, but {offered:?}")]
    MenuOffers {
        text: String,
        missing: Vec<ChatMenuItem>,
        offered: Vec<ChatMenuItem>,
    },
    #[error("no Add for the folder {folder:?}")]
    NoAdd { folder: String },
    #[error("no Approve for {member:?}")]
    NoApprove { member: String },
    #[error("no {row:?} row")]
    NoRow { row: String },
    /// `level` describes the nodes level with the row's text.
    #[error("the switch of {row:?} isn't on; level with it: {}", level.join("; "))]
    SwitchOff { row: String, level: Vec<String> },
    #[error("no title {group:?}")]
    NoTitle { group: String },
    #[error("no {button:?} by {message:?}")]
    NoButton { button: String, message: String },
    #[error("{number}: only +1 numbers, the default country")]
    NotPlusOne { number: String },
    #[error("not one switch for ANR crashes: {count}")]
    Switches { count: usize },
    #[error("no message field")]
    NoMessageField,
}

/// A wait on logcat's lines that failed ([`Logcat`]).
#[derive(Debug, thiserror::Error)]
enum LogcatError {
    #[error("logcat's follower panicked")]
    Poisoned,
    #[error("logcat's line count")]
    Count(#[source] std::num::TryFromIntError),
    #[error("the logcat mark {after}")]
    Mark {
        after: u64,
        #[source]
        source: std::num::TryFromIntError,
    },
    #[error("no {needle:?} in logcat within {} ms", patience.as_millis())]
    Timeout { needle: String, patience: Duration },
}

/// What keeps the driver from serving its socket.
#[derive(Debug, thiserror::Error)]
enum ServeError {
    #[error("clearing the driver's socket")]
    Clear(#[source] FileError),
    #[error("listening on {}", path.display())]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("making {} the owner's only", path.display())]
    Permissions {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("accepting a connection")]
    Accept(#[source] std::io::Error),
}

/// What ends a connection to the driver.
#[derive(Debug, thiserror::Error)]
enum ConnectionError {
    #[error("a second handle to the connection")]
    Clone(#[source] std::io::Error),
    #[error("reading a call")]
    Read(#[source] std::io::Error),
    #[error("serializing an answer")]
    Serialize(#[source] serde_json::Error),
    #[error("answering")]
    Write(#[source] std::io::Error),
}

fn main() -> std::process::ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match arguments.as_slice() {
        [] => start(),
        [command] if command == "config" => config(),
        [command, metadata, group, version, out] if command == "verification" => verification(
            Path::new(metadata),
            Path::new(group),
            version,
            Path::new(out),
        ),
        // Beside a running driver, which serves one connection at a time: `cargo testbox qr`.
        [show, url] if show == "show-qr" => show_qr(url).map_err(Failure::from),
        _ => Err(UsageError.into()),
    };
    testbox_tools::exit("android", outcome)
}

/// Creates `path` and its parents.
fn create_dir_all(path: &Path) -> Result<(), StageError> {
    std::fs::create_dir_all(path).map_err(|source| StageError::CreateDirectory {
        path: path.to_owned(),
        source,
    })
}

/// The image's `config` stage: the local flavor's values the APK is built with for the box's keys
/// (testbox/android/Containerfile; the keys come from the server image). The same bytes
/// for the same keys, so that Gradle builds again only for new keys.
fn config() -> Result<(), Failure> {
    let (_config, text) = local_config_json()?;
    let out = Path::new(CONFIG_OUT);
    create_dir_all(out)?;
    Ok(write(&out.join(CONFIG), text)?)
}

/// The image's `verification` stage: Signal-Android's `metadata` with the patched libsignal's
/// artifacts in (`testbox_tools::verification`), from `group` (a Maven repository's
/// `org/signal`) at `version`, into `out`. The same bytes for the same artifacts, so that Gradle
/// builds again only for new ones.
fn verification(metadata: &Path, group: &Path, version: &str, out: &Path) -> Result<(), Failure> {
    let components = testbox_tools::verification::components(group, version)?;
    let patched =
        testbox_tools::verification::with_components(&read(metadata)?, version, &components)?;
    if let Some(dir) = out.parent() {
        create_dir_all(dir)?;
    }
    Ok(write(out, patched)?)
}

fn start() -> Result<(), Failure> {
    let socket = testbox_tools::env_var("ANDROID_DRIVER_SOCKET")?;
    let (config, text) = local_config_json()?;
    if read(Path::new(APK_CONFIG))? != text {
        return Err(LocalConfigError::OtherKeys {
            path: APK_CONFIG.into(),
        }
        .into());
    }
    boot_emulator(&config)?;
    let launcher = install(Path::new(APK))?;
    let mut driver = Driver {
        launcher,
        agent: Agent::start()?,
    };
    no_soft_keyboard()?;
    gesture_navigation()?;
    driver.launch()?;
    Ok(serve(&mut driver, Path::new(&socket))?)
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

/// Desktop's values (testbox/tools `provision`), but Jetty's HTTP/2 port for the app's REST
/// requests; the chat takes the omnibus port, as Desktop's does.
fn local_config() -> Result<LocalConfig, LocalConfigError> {
    let desktop_path = KeyFile::Desktop.path();
    let desktop: DesktopConfig = serde_json::from_str(
        &read(&desktop_path).map_err(LocalConfigError::Keys)?,
    )
    .map_err(|source| LocalConfigError::Desktop {
        path: desktop_path.clone(),
        source,
    })?;
    let ca_path = KeyFile::TlsCa.path();
    let der = keys::pem_body(
        &read(&ca_path).map_err(LocalConfigError::Keys)?,
        "CERTIFICATE",
    )
    .ok_or(LocalConfigError::NoCertificate { path: ca_path })?;
    Ok(LocalConfig {
        service_url: keys::localhost(keys::LIBSIGNAL_NET_PORT),
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
            .ok_or(LocalConfigError::NoTrustRoot { path: desktop_path })?,
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
fn local_config_json() -> Result<(LocalConfig, String), LocalConfigError> {
    let config = local_config()?;
    let text = serde_json::to_string_pretty(&config).map_err(LocalConfigError::Serialize)?;
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
            Err(error) => println!("adb {}: {}", part.join(" "), testbox_tools::Report(&error)),
        }
    }
}

/// How often `adb root` or `adb unroot` is tried, each time once the device is back.
const ROOT_TRIES: u32 = 5;

/// `adb root` (`switch` "root") or `adb unroot`, once adb reaches the device. Right after boot,
/// and right after the framework restarts ([`raise_anr_timeouts`]), the emulator's transport can
/// go offline for a moment (`adb devices`: `offline`; the emulator's own `adb shell` fails then
/// too), and adb answers "unable to connect for root: closed" (or "for unroot"): tried again
/// once it is back.
fn switch_root(switch: &str) -> Result<(), CommandError> {
    let mut tries = 0;
    loop {
        // `wait-for-device` waits for the `device` state, past `offline`.
        adb(&["wait-for-device"])?;
        match adb(&[switch]) {
            Ok(_) => return Ok(()),
            Err(error) if tries + 1 < ROOT_TRIES => {
                tries += 1;
                println!(
                    "android: adb {switch}: {}; again once the device is back",
                    testbox_tools::Report(&error)
                );
            }
            Err(error) => {
                // What the device says, for the next time.
                print_device_state();
                return Err(error);
            }
        }
    }
}

fn root() -> Result<(), BootError> {
    switch_root("root").map_err(BootError::Root)
}

fn adb(args: &[&str]) -> Result<String, CommandError> {
    let mut command = Command::new("adb");
    command.args(args);
    output(command, "")
}

/// Starts `command` with its stdout piped, which it returns.
fn spawn_piped(command: &mut Command) -> Result<(Child, ChildStdout), SpawnError> {
    let mut child = command
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|source| SpawnError::Start {
            command: describe(command),
            source,
        })?;
    let stdout = child.stdout.take().ok_or_else(|| SpawnError::NoStdout {
        command: describe(command),
    })?;
    Ok((child, stdout))
}

fn back() -> Result<(), UiError> {
    const KEY: &str = "KEYCODE_BACK";
    adb(&["shell", "input", "keyevent", KEY])
        .map(drop)
        .map_err(|source| UiError::Key { key: KEY, source })
}

/// The screen's size in pixels (`wm size`, "Physical size: 720x1560", an override's line after
/// it if there is one).
fn screen_size() -> Result<(u32, u32), UiError> {
    let size = adb(&["shell", "wm", "size"]).map_err(UiError::ScreenSize)?;
    size.lines()
        .filter_map(|line| line.split_once(": "))
        .filter_map(|(_, size)| size.trim().split_once('x'))
        .filter_map(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .next_back()
        .ok_or_else(|| UiError::ScreenSizeAnswer {
            answer: size.clone(),
        })
}

/// The screen as pixels (`screencap` without `-p`: width, height, format and colour space as
/// u32, then RGBA).
struct Screenshot {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fn screenshot() -> Result<Screenshot, UiError> {
    let taken = Command::new("adb")
        .args(["exec-out", "screencap"])
        .output()
        .map_err(UiError::Screencap)?;
    if !taken.status.success() || taken.stdout.len() < 16 {
        return Err(UiError::ScreencapFailed {
            status: taken.status,
            length: taken.stdout.len(),
        });
    }
    let word = |at: usize| u32::from_le_bytes(taken.stdout[at..at + 4].try_into().unwrap());
    let (width, height, format) = (word(0), word(4), word(8));
    if format != 1 {
        return Err(UiError::ScreencapFormat { format });
    }
    let bytes = (width as usize) * (height as usize) * 4;
    let start = taken.stdout.len() - bytes;
    Ok(Screenshot {
        width,
        height,
        pixels: taken.stdout[start..].to_vec(),
    })
}

impl Screenshot {
    /// The middle of the red heart emoji, the reaction bar's first: the red pixels (the
    /// heart's (255, 66, 54)) are nearly all its, on the dimmed screen below the app's header;
    /// the other emoji have a few. Their median, if most of them lie within an emoji of it.
    fn heart(&self) -> Option<(u32, u32)> {
        let header = self.height / 6;
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for y in header..self.height {
            for x in 0..self.width {
                let at = ((y * self.width + x) * 4) as usize;
                let (r, g, b) = (self.pixels[at], self.pixels[at + 1], self.pixels[at + 2]);
                if r >= 200 && g <= 110 && b <= 110 {
                    xs.push(x);
                    ys.push(y);
                }
            }
        }
        if xs.len() < 100 {
            return None;
        }
        xs.sort_unstable();
        ys.sort_unstable();
        let (x, y) = (xs[xs.len() / 2], ys[ys.len() / 2]);
        let emoji = self.width / 12;
        let near = xs
            .iter()
            .zip(&ys)
            .filter(|(px, py)| px.abs_diff(x) <= emoji && py.abs_diff(y) <= emoji)
            .count();
        (near * 10 >= xs.len() * 7).then_some((x, y))
    }
}

/// Multiplies Android's ANR timeouts (`Build.HW_TIMEOUT_MULTIPLIER`; `InputConstants`
/// `DEFAULT_DISPATCHING_TIMEOUT_MILLIS`: input 5 s to 30 s), so Signal's stalls on the emulator
/// bring no "isn't responding" dialog. `ro.hw_timeout_multiplier` is read-only once set and only
/// vendor_init may set it, but this image leaves it unset and makes adb's root shell permissive
/// (its sepolicy: `(typepermissive su)`); the emulator's `-prop` takes only `qemu.*`. The
/// framework reads it as it starts, so this restarts it.
fn raise_anr_timeouts() -> Result<(), BootError> {
    let adb = |args: &[&str]| adb(args).map_err(BootError::Timeouts);
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
        return Err(BootError::Multiplier {
            seen: multiplier,
            wanted: HW_TIMEOUT_MULTIPLIER,
        });
    }
    adb(&["shell", "stop"])?;
    // Only this start's lines: the first boot's are cleared.
    adb(&["logcat", "-b", "events", "-c"])?;
    let mut command = Command::new("adb");
    command.args(["logcat", "-b", "events"]);
    let (mut events, stdout) = spawn_piped(&mut command).map_err(BootError::EventLog)?;
    let started = announced(stdout, "Android's event log", "events: ", FRAMEWORK_STARTED);
    adb(&["shell", "start"])?;
    let heard = started.recv().map_err(|_| BootError::EventLogEnded);
    let stopped = |source| BootError::Stop {
        command: describe(&command),
        source,
    };
    events.kill().map_err(stopped)?;
    events.wait().map_err(stopped)?;
    heard?;
    // The framework just started again, and the transport with it: as `root`, once it is back.
    switch_root("unroot").map_err(BootError::Timeouts)?;
    adb(&["wait-for-device"]).map(drop)
}

/// The AVD's locks from an emulator of this container before it restarted (`restartPolicy:
/// Always`): its processes went with it, and the emulator refuses an AVD it finds locked
/// ("Running multiple emulators with the same AVD").
fn clear_avd_locks(avd: &Path) -> Result<(), BootError> {
    let listed = |source| BootError::ListAvd {
        path: avd.to_owned(),
        source,
    };
    for entry in std::fs::read_dir(avd).map_err(listed)? {
        let path = entry.map_err(listed)?.path();
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
fn remove_lock(path: &Path) -> Result<(), BootError> {
    let removed = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.map_err(|source| BootError::RemoveLock {
        path: path.to_owned(),
        source,
    })
}

/// `url`'s QR on the wall poster in the camera's scene (testbox/android/Toren1BD.posters).
fn show_qr(url: &str) -> Result<(), CommandError> {
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
fn configure_avd(avd: &Path) -> Result<(), BootError> {
    let path = avd.join("config.ini");
    let mut config = avd::Config::parse(&read(&path).map_err(BootError::AvdFile)?);
    let size = env_var(testbox_tools::DISPLAY_SIZE).map_err(BootError::AvdEnvironment)?;
    config
        .for_box(&size)
        .map_err(|source| BootError::AvdConfig {
            path: path.clone(),
            source,
        })?;
    write(&path, config.to_string()).map_err(BootError::AvdFile)
}

/// The device at the scale the AVD's user config names, which the emulator caps at 80% of the
/// display (its default leaves most of the display black). Read at start
/// (`user_config_get_window_scale`), written at exit.
fn set_window_scale(avd: &Path) -> Result<(), BootError> {
    let user_config = avd.join("emulator-user.ini");
    write(
        &user_config,
        "window.x = 0\nwindow.y = 0\nwindow.scale = 1.0\n",
    )
    .map_err(BootError::AvdFile)
}

fn boot_emulator(config: &LocalConfig) -> Result<(), BootError> {
    let home = env_var("HOME").map_err(BootError::AvdEnvironment)?;
    let avd = Path::new(&home).join(".android/avd/box.avd");
    clear_avd_locks(&avd)?;
    configure_avd(&avd)?;
    set_window_scale(&avd)?;
    // The adb server first, so it finds the emulator as it comes up.
    adb(&["start-server"]).map_err(BootError::AdbServer)?;
    let mut command = Command::new("emulator");
    command
        .args(["-avd", "box", "-no-audio", "-no-boot-anim", "-no-snapshot"])
        .args([
            "-no-metrics",
            "-gpu",
            "swiftshader_indirect",
            "-memory",
            "3072",
        ])
        .args(["-camera-back", "virtualscene"]);
    let (mut emulator, stdout) = spawn_piped(&mut command).map_err(BootError::Emulator)?;
    let boot = announced(stdout, "the emulator's output", "", BOOTED);
    // The channel closes when the emulator's output ends: it exited.
    boot.recv().map_err(|_| match emulator.try_wait() {
        Ok(Some(status)) => BootError::EmulatorExited { status },
        Ok(None) => BootError::EmulatorOutputEnded,
        Err(error) => BootError::EmulatorGone(error),
    })?;
    adb(&["wait-for-device"]).map_err(BootError::BootCompleted)?;
    let completed =
        adb(&["shell", "getprop", "sys.boot_completed"]).map_err(BootError::BootCompleted)?;
    if completed.trim() != "1" {
        return Err(BootError::NotBooted {
            boot_completed: completed,
        });
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
        adb(&["reverse", &tcp, &tcp]).map_err(|source| BootError::Reverse { port, source })?;
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
fn no_soft_keyboard() -> Result<(), BootError> {
    setting("secure", "show_ime_with_hard_keyboard", "0")
}

/// Navigation by gestures, a thin bar, in place of the three buttons' bar: the emulator's default
/// bar lies over the bottom of the app's windows, where Signal puts a story's Share and Send,
/// and the agent refuses a tap another window would take.
fn gesture_navigation() -> Result<(), BootError> {
    adb(&[
        "shell",
        "cmd",
        "overlay",
        "enable",
        "com.android.internal.systemui.navbar.gestural",
    ])
    .map(drop)
    .map_err(BootError::GestureNavigation)
}

/// Sets `name` in `namespace` (`settings put`) to `value`, and reads it back.
fn setting(namespace: &str, name: &str, value: &str) -> Result<(), BootError> {
    let failed = |source| BootError::Setting {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        source,
    };
    adb(&["shell", "settings", "put", namespace, name, value]).map_err(failed)?;
    let now = adb(&["shell", "settings", "get", namespace, name]).map_err(failed)?;
    if now.trim() == value {
        Ok(())
    } else {
        Err(BootError::SettingReads {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            seen: now,
            wanted: value.to_owned(),
        })
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
fn install(apk: &Path) -> Result<String, InstallError> {
    let path = apk.display().to_string();
    adb(&["install", "-r", "-g", &path]).map_err(|source| InstallError::Install {
        apk: apk.to_owned(),
        source,
    })?;
    let packages = adb(&["shell", "pm", "list", "packages", PACKAGE_PREFIX])
        .map_err(InstallError::Packages)?;
    let local: Vec<&str> = packages
        .split_whitespace()
        .filter_map(|line| line.strip_prefix("package:"))
        .filter(|package| package.ends_with(".local"))
        .collect();
    let [package] = local.as_slice() else {
        return Err(InstallError::NotOneLocalPackage {
            packages: local.iter().map(|&package| package.to_owned()).collect(),
        });
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
    ])
    .map_err(|source| InstallError::Launcher {
        package: (*package).to_owned(),
        source,
    })?;
    resolved
        .split_whitespace()
        .last()
        .map(str::to_owned)
        .ok_or_else(|| InstallError::NoLauncher {
            package: (*package).to_owned(),
        })
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
    fn follow() -> Result<(), BootError> {
        let mut command = Command::new("adb");
        command.args(["logcat", "-v", "time"]);
        let (_logcat, stdout) = spawn_piped(&mut command).map_err(BootError::Logcat)?;
        // A line at a time, as it is read: whole lines in the file for the box's logs.
        let file = std::fs::File::create(LOGCAT).map_err(|source| BootError::CreateLog {
            path: LOGCAT.into(),
            source,
        })?;
        let mut file = LineWriter::new(file);
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

    fn lines(&self) -> Result<MutexGuard<'_, Vec<String>>, LogcatError> {
        self.lines.lock().map_err(|_poisoned| LogcatError::Poisoned)
    }

    fn mark(&self) -> Result<u64, LogcatError> {
        let count = self.lines()?.len();
        u64::try_from(count).map_err(LogcatError::Count)
    }

    /// Waits, woken by each new line, until a line after `after` contains `needle`.
    fn await_line(&self, after: u64, needle: &str, patience: Duration) -> Result<(), LogcatError> {
        // Each wake looks at the lines that came since the last one only.
        let mut scanned =
            usize::try_from(after).map_err(|source| LogcatError::Mark { after, source })?;
        let (_lines, waited) = self
            .grew
            .wait_timeout_while(self.lines()?, patience, |lines| {
                let found = lines
                    .get(scanned..)
                    .is_some_and(|new| new.iter().any(|line| line.contains(needle)));
                scanned = scanned.max(lines.len());
                !found
            })
            .map_err(|_poisoned| LogcatError::Poisoned)?;
        if waited.timed_out() {
            Err(LogcatError::Timeout {
                needle: needle.to_owned(),
                patience,
            })
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
    fn start() -> Result<Self, AgentStartError> {
        adb(&["install", "-r", AGENT_APK]).map_err(|source| AgentStartError::Install {
            apk: AGENT_APK.into(),
            source,
        })?;
        let mut command = Command::new("adb");
        command.args([
            "shell",
            "am",
            "instrument",
            "-w",
            "-r",
            AGENT_INSTRUMENTATION,
        ]);
        let (instrumentation, stdout) =
            spawn_piped(&mut command).map_err(AgentStartError::Instrumentation)?;
        announced(stdout, "the agent's output", "agent: ", AGENT_ABSTRACT)
            .recv()
            .map_err(|_closed| AgentStartError::NotListening)?;
        remove_if_present(Path::new(AGENT_SOCKET)).map_err(AgentStartError::Socket)?;
        adb(&[
            "forward",
            &format!("localfilesystem:{AGENT_SOCKET}"),
            &format!("localabstract:{AGENT_ABSTRACT}"),
        ])
        .map_err(AgentStartError::Forward)?;
        let stream =
            UnixStream::connect(AGENT_SOCKET).map_err(|source| AgentStartError::Connect {
                path: AGENT_SOCKET.into(),
                source,
            })?;
        Ok(Self {
            instrumentation,
            calls: BufReader::new(stream),
            next_id: 1,
        })
    }

    /// A wait's answer: the screen it found.
    fn look(&mut self, call: agent::Call) -> Result<agent::Found, AgentError> {
        match self.call(call)? {
            (_, agent::Outcome::Done(found)) => Ok(found),
            (call, outcome) => Err(AgentError::Unexpected {
                call,
                outcome: Box::new(outcome),
            }),
        }
    }

    /// An action's answer: done.
    fn act(&mut self, call: agent::Call) -> Result<(), AgentError> {
        match self.call(call)? {
            (_, agent::Outcome::Acted) => Ok(()),
            (call, outcome) => Err(AgentError::Unexpected {
                call,
                outcome: Box::new(outcome),
            }),
        }
    }

    /// Taps (or long-presses) `node` of the last answer's screen.
    fn click(&mut self, node: &agent::Node, long: bool) -> Result<(), AgentError> {
        self.act(agent::Call::Click {
            handle: node.handle,
            long,
        })
    }

    /// The agent's answer to `call`, and the call back, for what the caller makes of it.
    fn call(&mut self, call: agent::Call) -> Result<(agent::Call, agent::Outcome), AgentError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = agent::Request { id, call };
        let line = match serde_json::to_string(&request) {
            Ok(line) => line,
            Err(source) => {
                return Err(AgentError::Serialize {
                    call: request.call,
                    source,
                });
            }
        };
        let call = request.call;
        if let Err(source) = writeln!(self.calls.get_mut(), "{line}") {
            return Err(AgentError::Send { call, source });
        }
        let mut reply = String::new();
        let read = match self.calls.read_line(&mut reply) {
            Ok(read) => read,
            Err(source) => return Err(AgentError::Receive { call, source }),
        };
        if read == 0 {
            return Err(match self.instrumentation.try_wait() {
                Ok(status) => AgentError::HungUp { call, status },
                Err(source) => AgentError::Instrumentation { call, source },
            });
        }
        let reply: agent::Reply = match serde_json::from_str(&reply) {
            Ok(parsed) => parsed,
            Err(source) => {
                return Err(AgentError::Reply {
                    call,
                    reply,
                    source,
                });
            }
        };
        match reply.outcome {
            agent::Outcome::Failed(message) => Err(AgentError::Failed { call, message }),
            _ if reply.id != id => Err(AgentError::OtherCall {
                call,
                id,
                answered: reply.id,
            }),
            outcome => Ok((call, outcome)),
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
    fn answer(&mut self, call: Android) -> Result<Answered, Failure> {
        let done = match call {
            Android::Register { number, name } => self.register(&number, &name),
            Android::NoteToSelf { text } => self.note_to_self(&text),
            Android::LinkDevice { url, transfer } => self.link_device(&url, transfer),
            Android::OpenConversation { text } => {
                self.launch()?;
                self.open_row(&text)
            }
            Android::Tap { text } => self.tap(&text, PATIENCE),
            Android::LongPress { text } => self.long_press(&text).map(drop),
            Android::Back => back(),
            Android::TypeText { text } => self.type_text(&text, agent::Echo::Exact(text.clone())),
            Android::ReactToMessage { text } => self.react_to_message(&text),
            Android::SetTimer { seconds } => self.set_timer(seconds),
            Android::SetNickname { given } => self.set_nickname(&given),
            Android::PostStory { group, text } => self.post_story(&group, &text),
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
            Android::LogMark => return Ok(LOGCAT_LINES.mark().map(Answered::Mark)?),
            Android::AwaitStorageSync { after, timeout_ms } => {
                LOGCAT_LINES.await_line(
                    after,
                    STORAGE_SYNCED,
                    Duration::from_millis(timeout_ms),
                )?;
                Ok(())
            }
        };
        Ok(done.map(|()| Answered::Done)?)
    }

    /// Long-presses the message with `text`, which opens its menu and, beside the message, the
    /// reaction bar (ConversationReactionOverlay.java): a row of emoji drawn without labels,
    /// chosen by touch, with the message scrolled under it. The first emoji is ❤️, the
    /// account's first preferred reaction: the one patch of red on the dimmed screen, found on
    /// a screenshot and tapped.
    fn react_to_message(&mut self, text: &str) -> Result<(), UiError> {
        self.long_press(text)?;
        // The bar slides into place after the menu shows.
        std::thread::sleep(Duration::from_millis(600));
        let shot = screenshot()?;
        // Kept for reading afterwards (`podman cp`), with the tap's place in the log.
        let kept = "/tmp/reaction.png";
        if let Ok(png) = Command::new("adb")
            .args(["exec-out", "screencap", "-p"])
            .output()
            && let Err(error) = std::fs::write(kept, png.stdout)
        {
            eprintln!("android: couldn't keep {kept}: {error}");
        }
        let (x, y) = shot.heart().ok_or_else(|| UiError::NoReactionBar {
            text: text.to_owned(),
            kept: kept.to_owned(),
        })?;
        eprintln!("android: the reaction bar's heart at ({x}, {y}); tapping it");
        adb(&["shell", "input", "tap", &x.to_string(), &y.to_string()])
            .map_err(|source| UiError::TapAt { x, y, source })?;
        // The overlay goes with the tap; its menu too.
        Ok(self.agent.act(agent::Call::WaitGone {
            wanted: vec![agent::Wanted::label("Reply")],
            timeout_ms: timeout_ms(PATIENCE),
        })?)
    }

    /// Taps the chat list's row showing `text`, scrolled to if the list is long.
    fn open_row(&mut self, text: &str) -> Result<(), UiError> {
        if self
            .first_shown(vec![agent::Wanted::control(text)], GLANCE)
            .is_err()
        {
            let row = self.agent.look(agent::Call::ScrollTo {
                label: text.to_owned(),
                timeout_ms: timeout_ms(PATIENCE),
            })?;
            return self.tap_at(&row.node);
        }
        self.tap(text, PATIENCE)
    }

    /// Long-presses the message with `text` in it, and waits for its menu; a menu left open
    /// by a call before is closed first, as it would take the press.
    fn long_press(&mut self, text: &str) -> Result<agent::Found, UiError> {
        // A message's menu, or a text selection's bar, left by a call before.
        let open = || {
            vec![
                agent::Wanted::label("Forward"),
                agent::Wanted::label("Message details"),
            ]
        };
        if self.first_shown(open(), GLANCE).is_ok() {
            back()?;
            self.agent.act(agent::Call::WaitGone {
                wanted: open(),
                timeout_ms: timeout_ms(PATIENCE),
            })?;
        }
        let found = self.first_shown(vec![agent::Wanted::containing(text)], PATIENCE)?;
        self.agent.click(&found.node, true)?;
        self.element("Reply", PATIENCE)?;
        Ok(found)
    }

    /// The open conversation's timer, from its chat settings (ConversationSettingsFragment,
    /// "Disappearing messages": a radio list of the app's presets and Save, strings.xml
    /// `ExpirationDialog_*`); then back to the conversation.
    fn set_timer(&mut self, seconds: u32) -> Result<(), UiError> {
        let label = match seconds {
            0 => "Off",
            2_419_200 => "4 weeks",
            604_800 => "1 week",
            86_400 => "1 day",
            28_800 => "8 hours",
            3_600 => "1 hour",
            300 => "5 minutes",
            30 => "30 seconds",
            seconds => return Err(UiError::Preset { seconds }),
        };
        self.tap("More options", PATIENCE)?;
        self.tap("Chat settings", PATIENCE)?;
        self.tap("Disappearing messages", PATIENCE)?;
        self.element("Custom time", PATIENCE)?;
        self.tap(label, PATIENCE)?;
        self.tap("Save", PATIENCE)?;
        self.back_to_conversation()
    }

    /// Chat settings' "Nickname" (IndividualSettingsScreen.kt, strings.xml
    /// `NicknameActivity__nickname`), its "First name" filled, "Save".
    fn set_nickname(&mut self, given: &str) -> Result<(), UiError> {
        self.tap("More options", PATIENCE)?;
        self.tap("Chat settings", PATIENCE)?;
        self.tap("Nickname", PATIENCE)?;
        let field = self.element("First name", PATIENCE)?;
        self.fill(&field.node, given, agent::Echo::Exact(given.to_owned()))?;
        self.tap("Save", PATIENCE)?;
        self.back_to_conversation()
    }

    /// Back until the conversation's composer shows (its content description, strings.xml
    /// `conversation_activity__compose_message`).
    fn back_to_conversation(&mut self) -> Result<(), UiError> {
        for _ in 0..4 {
            if self
                .first_shown(vec![agent::Wanted::label("Message composition")], GLANCE)
                .is_ok()
            {
                return Ok(());
            }
            back()?;
        }
        Err(UiError::NotBackInConversation)
    }

    /// A text story to `group`: the Stories tab's camera, its Text mode, the text typed and
    /// done, the editor's next arrow (unlabelled: at its place on the screen), the group chosen
    /// on "Send to", Send; then the chats.
    fn post_story(&mut self, group: &str, text: &str) -> Result<(), UiError> {
        self.launch()?;
        self.tap("Stories", PATIENCE)?;
        self.tap("Open Camera", PATIENCE)?;
        self.tap("Text", PATIENCE)?;
        self.tap("Tap to add text", PATIENCE)?;
        self.element("Done adding text", PATIENCE)?;
        self.type_text(text, agent::Echo::Exact(text.to_owned()))?;
        self.tap("Done adding text", PATIENCE)?;
        self.element("Add link", PATIENCE)?;
        // The arrow's place with the gesture navigation bar (`gesture_navigation`): bottom
        // right, above the bar.
        let (width, height) = screen_size()?;
        let (x, y) = (width * 91 / 100, height * 925 / 1000);
        adb(&["shell", "input", "tap", &x.to_string(), &y.to_string()])
            .map_err(|source| UiError::TapAt { x, y, source })?;
        self.element("Send to", PATIENCE)?;
        // A group the account never posted to isn't offered: it is added as a group story
        // (New, Group story, its introduction the first time, the group, Share), and comes
        // back chosen.
        if self
            .first_shown(vec![agent::Wanted::control(group)], GLANCE)
            .is_ok()
        {
            self.tap(group, PATIENCE)?;
        } else {
            self.tap("New", PATIENCE)?;
            self.tap("Group story", PATIENCE)?;
            let shown = self.first_shown(
                vec![
                    agent::Wanted::label("Choose groups"),
                    agent::Wanted::control("Next"),
                ],
                PATIENCE,
            )?;
            if shown.which == 1 {
                self.tap_at(&shown.node)?;
                self.element("Choose groups", PATIENCE)?;
            }
            self.tap(group, PATIENCE)?;
            self.tap("Share", PATIENCE)?;
        }
        self.tap("Send", PATIENCE)?;
        self.agent.act(agent::Call::WaitGone {
            wanted: vec![agent::Wanted::label("Send to")],
            timeout_ms: timeout_ms(PATIENCE),
        })?;
        self.launch()
    }

    /// Opens the menu of the chat list's row showing `text` (a long click on it); the menu's own
    /// window, by its last item, Delete, which shows with all of them (ConversationListFragment.java
    /// `onConversationLongClick`, strings.xml `ConversationListFragment_delete`): its items, not
    /// their namesakes behind it.
    fn open_chat_menu(&mut self, text: &str) -> Result<Vec<agent::Node>, UiError> {
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

    fn chat_menu(&mut self, text: &str, item: ChatMenuItem) -> Result<(), UiError> {
        let menu = self.open_chat_menu(text)?;
        let entry = menu
            .iter()
            .find(|node| node.labelled(item.label()))
            .ok_or_else(|| UiError::MenuItem {
                text: text.to_owned(),
                item,
            })?;
        self.tap_at(entry)?;
        if item == ChatMenuItem::Mute {
            // MuteDialog.kt's last option (`arrays__always`).
            self.tap("Always", PATIENCE)?;
        }
        Ok(())
    }

    /// Opens the row's menu once and reads it: a menu shows the chat as it was when opened, so
    /// the caller waits for what it expects to have happened first ([`Android::AwaitStorageSync`]).
    fn chat_menu_offers(&mut self, text: &str, items: &[ChatMenuItem]) -> Result<(), UiError> {
        let menu = self.open_chat_menu(text)?;
        back()?;
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
        let missing: Vec<ChatMenuItem> = items
            .iter()
            .copied()
            .filter(|item| !offered.contains(item))
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(UiError::MenuOffers {
                text: text.to_owned(),
                missing,
                offered,
            })
        }
    }

    /// Settings, Chats, "Add a chat folder", then the `Add` in the suggestion's row. Whether the
    /// folder came is for its storage sync to show (the xtask waits for it in b33p): the app's
    /// own word, a snackbar (`ChatFoldersFragment__folder_added`), is gone in a moment.
    fn add_suggested_folder(&mut self, name: &str) -> Result<(), UiError> {
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
            .ok_or_else(|| UiError::NoAdd {
                folder: name.to_owned(),
            })?;
        self.tap_at(add)
    }

    /// The group's settings, the member's sheet, its item, then the confirmation.
    fn group_member(
        &mut self,
        group: &str,
        member: &str,
        change: MemberChange,
    ) -> Result<(), UiError> {
        self.group_settings_row(group, member)?;
        self.tap(change.item(), PATIENCE)?;
        let (message, button) = change.confirmation();
        self.confirm(message, button)
    }

    /// The invite link's page (ShareableGroupLinkFragment): its switch, then, once the link is on,
    /// the approval's, each by its row's label, whose tap toggles it.
    fn group_link(&mut self, group: &str, approval: bool) -> Result<(), UiError> {
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
            .ok_or_else(|| UiError::NoRow {
                row: "Group link".to_owned(),
            })?;
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
    fn switch_on(&mut self, row: &str) -> Result<(), UiError> {
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
                .ok_or_else(|| UiError::NoRow {
                    row: row.to_owned(),
                })?;
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
                return Err(UiError::SwitchOff {
                    row: row.to_owned(),
                    level: nodes,
                });
            }
        }
    }

    /// The request among "Requests & invites" (PendingMembersActivity, strings.xml
    /// `PendingMembersActivity_requests`): its Approve beside `member`'s name
    /// (`GroupRecipientListItem_approve_description`), then the confirmation's Add
    /// (`RequestConfirmationDialog_add_s_to_the_group`, `RequestConfirmationDialog_add`).
    fn approve_request(&mut self, group: &str, member: &str) -> Result<(), UiError> {
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
            .ok_or_else(|| UiError::NoApprove {
                member: member.to_owned(),
            })?;
        self.tap_at(approve)?;
        self.confirm("to the group?", "Add")
    }

    /// The group `group` names, opened at its settings, scrolled to `row`, which is tapped.
    fn group_settings_row(&mut self, group: &str, row: &str) -> Result<(), UiError> {
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
            .ok_or_else(|| UiError::NoTitle {
                group: group.to_owned(),
            })?;
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
    fn confirm(&mut self, message: &str, button: &str) -> Result<(), UiError> {
        let dialog = self.first_shown(vec![agent::Wanted::containing(message)], PATIENCE)?;
        let confirm = dialog
            .screen
            .iter()
            .find(|node| node.window == dialog.node.window && node.labelled(button))
            .ok_or_else(|| UiError::NoButton {
                button: button.to_owned(),
                message: message.to_owned(),
            })?;
        self.tap_at(confirm)
    }

    /// The app on its chat list. If it shows another screen, or isn't running, from its launcher
    /// entry on a fresh task: its main screen, not whatever was last open
    /// (FLAG_ACTIVITY_NEW_TASK | FLAG_ACTIVITY_CLEAR_TASK). Not when it is on its chat list
    /// already: clearing the task while Compose still disposes what the list showed (a row's
    /// menu, closing) crashes Signal-Android now and then ("ConversationListFragment … is not
    /// currently in the FragmentManager"), and the list's controls going away doesn't say when
    /// that is done.
    fn launch(&mut self) -> Result<(), UiError> {
        // The chat list's own button (strings.xml `conversation_list_fragment__fab_content_description`).
        if self
            .first_shown(vec![agent::Wanted::control("New chat")], GLANCE)
            .is_ok()
        {
            return Ok(());
        }
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
        .map_err(|source| UiError::Launch {
            activity: self.launcher.clone(),
            source,
        })
    }

    /// The first of `wanted` to show, on the settled screen; a failure with the last screen after
    /// `patience`.
    fn first_shown(
        &mut self,
        wanted: Vec<agent::Wanted>,
        patience: Duration,
    ) -> Result<agent::Found, UiError> {
        Ok(self.agent.look(agent::Call::WaitFor {
            wanted,
            timeout_ms: timeout_ms(patience),
        })?)
    }

    fn element(&mut self, text: &str, patience: Duration) -> Result<agent::Found, UiError> {
        self.first_shown(vec![agent::Wanted::label(text)], patience)
    }

    /// Clicks `node` of the last answer's screen.
    fn tap_at(&mut self, node: &agent::Node) -> Result<(), UiError> {
        Ok(self.agent.click(node, false)?)
    }

    /// Taps the control labelled `text`, once it is enabled. A node that went between the look
    /// and the tap (the screen drew again, as Compose does on its own schedule) is looked for
    /// once more.
    fn tap(&mut self, text: &str, patience: Duration) -> Result<(), UiError> {
        let found = self.first_shown(vec![agent::Wanted::control(text)], patience)?;
        match self.tap_at(&found.node) {
            Err(UiError::Agent(ref error))
                if let AgentError::Failed { message, .. } = &**error
                    && message.ends_with(" went") =>
            {
                eprintln!("android: {message}; looking for {text:?} again");
                let found = self.first_shown(vec![agent::Wanted::control(text)], patience)?;
                self.tap_at(&found.node)
            }
            outcome => outcome,
        }
    }

    /// Sets the text of the field with input focus, once one has it, and waits until it reads as
    /// `echo` says: through accessibility, not keys, which go astray while the keyboard is still
    /// binding to a field just focused (a search for "ANR" arrives as "R", a phone number not at
    /// all).
    fn type_text(&mut self, text: &str, echo: agent::Echo) -> Result<(), UiError> {
        Ok(self.agent.act(agent::Call::SetText {
            text: text.to_owned(),
            echo,
            timeout_ms: timeout_ms(PATIENCE),
        })?)
    }

    /// Gives `field` focus, as a tap does, and sets its text.
    fn fill(&mut self, field: &agent::Node, text: &str, echo: agent::Echo) -> Result<(), UiError> {
        self.tap_at(field)?;
        self.type_text(text, echo)
    }

    /// Registers `number` (+1 only: the default country) through the app's own screens, with the
    /// box's captcha page and any code, the PIN disabled (no SVR in the box), and `name` as the
    /// profile's; then turns Signal's ANR crash off (`disable_anr_crashes`).
    fn register(&mut self, number: &str, name: &str) -> Result<(), UiError> {
        let national = number
            .strip_prefix("+1")
            .ok_or_else(|| UiError::NotPlusOne {
                number: number.to_owned(),
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
    fn disable_anr_crashes(&mut self) -> Result<(), UiError> {
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
            return Err(UiError::Switches {
                count: switches.len(),
            });
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
    fn link_device(&mut self, url: &str, transfer: bool) -> Result<(), UiError> {
        show_qr(url).map_err(|source| UiError::ShowQr {
            url: url.to_owned(),
            source,
        })?;
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
    fn note_to_self(&mut self, text: &str) -> Result<(), UiError> {
        self.launch()?;
        self.tap("New chat", PATIENCE)?;
        self.tap("Note to Self", PATIENCE)?;
        self.send(text)
    }

    /// Sends `text` in the open conversation, from its composer: the screen's last field.
    fn send(&mut self, text: &str) -> Result<(), UiError> {
        let filled = self.fill_message_field(text);
        // The field's node went between the look and the tap: look again, once.
        match filled {
            Err(UiError::Agent(ref error))
                if let AgentError::Failed { message, .. } = &**error
                    && message.ends_with(" went") =>
            {
                eprintln!("android: {message}; looking for the message field again");
                self.fill_message_field(text)?;
            }
            outcome => outcome?,
        }
        // The composer's button: "Send edit" while a message is being edited.
        let send = self.first_shown(
            vec![
                agent::Wanted::control("Send message"),
                agent::Wanted::control("Send edit"),
            ],
            PATIENCE,
        )?;
        self.tap_at(&send.node)
    }

    /// The composer's field, the last editable one on screen, given `text`.
    fn fill_message_field(&mut self, text: &str) -> Result<(), UiError> {
        let shown = self.first_shown(vec![agent::Wanted::Editable], PATIENCE)?;
        let field = shown
            .screen
            .into_iter()
            .rfind(|node| node.editable)
            .ok_or(UiError::NoMessageField)?;
        self.fill(&field, text, agent::Echo::Exact(text.to_owned()))
    }

    fn settings(&mut self) -> Result<(), UiError> {
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

fn serve(driver: &mut Driver, socket: &Path) -> Result<(), ServeError> {
    remove_if_present(socket).map_err(ServeError::Clear)?;
    let listener = UnixListener::bind(socket).map_err(|source| ServeError::Bind {
        path: socket.to_owned(),
        source,
    })?;
    // The owner's only, as b33p's harness socket is (b33p-slint/src/agent.rs): the socket's
    // directory is on the host.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        ServeError::Permissions {
            path: socket.to_owned(),
            source,
        }
    })?;
    println!("{} {}", drivers::LISTENING, socket.display());
    for stream in listener.incoming() {
        let stream = stream.map_err(ServeError::Accept)?;
        // A connection that fails ends; the driver serves the next.
        if let Err(error) = answer_calls(driver, stream) {
            eprintln!("android: {}", testbox_tools::Report(&error));
        }
    }
    Ok(())
}

fn answer_calls(driver: &mut Driver, stream: UnixStream) -> Result<(), ConnectionError> {
    let mut writer = stream.try_clone().map_err(ConnectionError::Clone)?;
    for line in BufReader::new(stream).lines() {
        let line = line.map_err(ConnectionError::Read)?;
        let answer = match serde_json::from_str::<Numbered<Android>>(&line) {
            Ok(Numbered { id, call }) => match driver.answer(call) {
                Ok(result) => Answer::Done { id, result },
                Err(error) => Answer::Failed {
                    id: Some(id),
                    // With its causes: the agent's own message (what the screen showed) is one.
                    error: testbox_tools::Report(&*error).to_string(),
                },
            },
            Err(error) => Answer::Failed {
                id: serde_json::from_str::<drivers::Id>(&line)
                    .ok()
                    .and_then(|call| call.id),
                error: format!("not a call: {error}: {line}"),
            },
        };
        let reply = serde_json::to_string(&answer).map_err(ConnectionError::Serialize)?;
        writeln!(writer, "{reply}").map_err(ConnectionError::Write)?;
    }
    Ok(())
}
