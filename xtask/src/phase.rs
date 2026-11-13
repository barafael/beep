//! `cargo testbox test`'s phases, by the box they run on: their names and the order they run in
//! are those of [`LinkedPhase`]'s, [`AndroidPhase`]'s, [`StressPhase`]'s, [`MeshPhase`]'s and
//! [`InteropPhase`]'s variants.

use std::sync::LazyLock;

use clap::ValueEnum;
use clap::builder::PossibleValue;
use strum::VariantArray;

/// The phases on the linked box (xtask/src/checks/setup.rs `LinkedBox`), in the order they run.
#[derive(Clone, Copy, PartialEq, Eq, strum::IntoStaticStr, strum::VariantArray)]
#[strum(serialize_all = "lowercase")]
pub(crate) enum LinkedPhase {
    Desktop,
    // Before anything has the primary receive: its profile picture check needs b33p to fetch
    // the peer's profile then, as presage keeps a profile it fetched for good.
    Attachments,
    // Impairs the clients' link to the servers, and makes it whole again at its end.
    Network,
    // Restarts the server.
    Core,
}

/// The phases on the Android box (`AndroidBox` there), in the order they run.
#[derive(Clone, Copy, PartialEq, Eq, strum::IntoStaticStr, strum::VariantArray)]
#[strum(serialize_all = "lowercase")]
pub(crate) enum AndroidPhase {
    Android,
    Groups,
    // Pins and archives the chat with the peer.
    Storage,
    // Twenty presage-cli members more: b33p's account's second group, a large one.
    #[strum(serialize = "large-group")]
    LargeGroup,
}

/// The stress phases (pending/stress.md), each on a box of its own (`PlainBox` there), and
/// only when named: they stop b33p and the server on purpose, and the soak takes half an hour.
#[derive(Clone, Copy, PartialEq, Eq, strum::IntoStaticStr, strum::VariantArray)]
pub(crate) enum StressPhase {
    // Bursts of messages, with b33p running and stopped.
    #[strum(serialize = "stress-volume")]
    Volume,
    // b33p killed while it receives, sends and downloads; the server hanging.
    #[strum(serialize = "stress-interrupt")]
    Interrupt,
    // Awkward texts, broken and huge pictures, odd file names.
    #[strum(serialize = "stress-content")]
    Content,
    // presage-cli, Desktop and b33p sending at once; races on one message.
    #[strum(serialize = "stress-concurrency")]
    Concurrency,
    // Mixed traffic for half an hour (`TESTBOX_SOAK_MINUTES`), from a seed (`TESTBOX_SEED`):
    // b33p's memory and open files mustn't keep growing.
    #[strum(serialize = "stress-soak")]
    Soak,
}

/// The phases on the mesh (xtask/src/checks/setup.rs `MeshBox`), several b33ps writing to each
/// other, and only when named.
#[derive(Clone, Copy, PartialEq, Eq, strum::IntoStaticStr, strum::VariantArray)]
#[strum(serialize_all = "lowercase")]
pub(crate) enum MeshPhase {
    // Every feature, in an order drawn from a seed (`TESTBOX_SEED`), each checked on every b33p
    // it reaches (xtask/src/checks/tour.rs).
    Tour,
}

/// The phases on the box of the clients that are not presage-cli, Desktop or Android (xtask/src/
/// checks/setup.rs `InteropBox`), and only when named: each client is another implementation of
/// Signal's protocol, built from its own source, and its image is a long build.
#[derive(Clone, Copy, PartialEq, Eq, strum::IntoStaticStr, strum::VariantArray)]
pub(crate) enum InteropPhase {
    // signal-cli (Java, libsignal-service-java): registers, links, sends and receives, in
    // groups too, with b33p and presage-cli.
    #[strum(serialize = "signal-cli")]
    SignalCli,
    /// mautrix-signal's signalmeow, in Go (xtask/src/checks/signalmeow.rs): text with a presage-cli
    /// account and with b33p, files, and a group. On a box of its own, set up by the phase itself.
    #[strum(serialize = "signalmeow")]
    Signalmeow,
    /// Every implementation at once (xtask/src/checks/mixed.rs): bursts that overlap between b33p,
    /// presage-cli, signal-cli and signalmeow, each side held to once and in order. On a box of
    /// its own, set up by the phase itself.
    #[strum(serialize = "mixed")]
    Mixed,
}

/// The presage family's clients (testbox/family), each on a box of its own, and only when named:
/// the box has b33p, a presage-cli peer and the client, which writes to both.
#[derive(Clone, Copy, PartialEq, Eq, strum::IntoStaticStr, strum::VariantArray)]
pub(crate) enum FamilyPhase {
    // gurk-rs, the terminal client, linked to a presage-cli primary; run in tmux.
    #[strum(serialize = "family-gurk")]
    Gurk,
    // Flare's backend (flare-manager), registered as an account of its own; one command at a time.
    #[strum(serialize = "family-flare")]
    Flare,
}

/// `test`'s phases, by the fixture they run on: the linked box's, the Android box's (one box each,
/// or with `--fresh` one per phase), then the stress phases', each on a box of its own.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Linked(LinkedPhase),
    Android(AndroidPhase),
    Stress(StressPhase),
    Mesh(MeshPhase),
    Interop(InteropPhase),
    Family(FamilyPhase),
}

/// Every phase, in the order they run.
static PHASES: LazyLock<Vec<Phase>> = LazyLock::new(|| {
    LinkedPhase::VARIANTS
        .iter()
        .copied()
        .map(Phase::Linked)
        .chain(AndroidPhase::VARIANTS.iter().copied().map(Phase::Android))
        .chain(StressPhase::VARIANTS.iter().copied().map(Phase::Stress))
        .chain(MeshPhase::VARIANTS.iter().copied().map(Phase::Mesh))
        .chain(InteropPhase::VARIANTS.iter().copied().map(Phase::Interop))
        .chain(FamilyPhase::VARIANTS.iter().copied().map(Phase::Family))
        .collect()
});

impl ValueEnum for Phase {
    fn value_variants<'a>() -> &'a [Self] {
        &PHASES
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        let name: &'static str = match *self {
            Self::Linked(phase) => phase.into(),
            Self::Android(phase) => phase.into(),
            Self::Stress(phase) => phase.into(),
            Self::Mesh(phase) => phase.into(),
            Self::Interop(phase) => phase.into(),
            Self::Family(phase) => phase.into(),
        };
        Some(PossibleValue::new(name))
    }
}
