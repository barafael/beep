//! `cargo testbox test`'s phases, by the box they run on: their names and the order they run in
//! are those of [`LinkedPhase`]'s, [`AndroidPhase`]'s and [`StressPhase`]'s variants.

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

/// `test`'s phases, by the box they run on: the linked box's, the Android box's, then the stress
/// phases', each on its own.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Linked(LinkedPhase),
    Android(AndroidPhase),
    Stress(StressPhase),
}

/// Every phase, in the order they run.
static PHASES: LazyLock<Vec<Phase>> = LazyLock::new(|| {
    LinkedPhase::VARIANTS
        .iter()
        .copied()
        .map(Phase::Linked)
        .chain(AndroidPhase::VARIANTS.iter().copied().map(Phase::Android))
        .chain(StressPhase::VARIANTS.iter().copied().map(Phase::Stress))
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
        };
        Some(PossibleValue::new(name))
    }
}
