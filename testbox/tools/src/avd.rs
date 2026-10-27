//! The AVD's `config.ini` (the emulator's `key=value` lines), set by the `android` tool before
//! the emulator starts.

use std::fmt;

use crate::{Error, Result};

/// The Pixel 5's width in pixels and its density (the AVD's device: testbox/android/Containerfile
/// `avdmanager create … -d pixel_5`). At another width the screen keeps its width in dp, and so
/// Android's layout.
const PIXEL_5: (u32, u32) = (1080, 440);

/// A `config.ini`, every line kept as it was but the settings changed.
pub struct Config(Vec<Line>);

enum Line {
    Setting { key: String, value: String },
    Other(String),
}

impl Config {
    pub fn parse(text: &str) -> Self {
        Self(
            text.lines()
                .map(|line| match line.split_once('=') {
                    Some((key, value)) => Line::Setting {
                        key: key.to_owned(),
                        value: value.to_owned(),
                    },
                    None => Line::Other(line.to_owned()),
                })
                .collect(),
        )
    }

    pub fn get(&self, key: &str) -> Result<&str> {
        self.0
            .iter()
            .find_map(|line| match line {
                Line::Setting { key: name, value } if name == key => Some(value.as_str()),
                _ => None,
            })
            .ok_or_else(|| Error::Failed(format!("config.ini has no {key}")))
    }

    /// Sets a setting the AVD has: one it doesn't have is a key misspelt, or an emulator that
    /// names it otherwise.
    pub fn set(&mut self, key: &str, to: &str) -> Result<()> {
        let value = self
            .0
            .iter_mut()
            .find_map(|line| match line {
                Line::Setting { key: name, value } if name == key => Some(value),
                _ => None,
            })
            .ok_or_else(|| Error::Failed(format!("config.ini has no {key}")))?;
        to.clone_into(value);
        Ok(())
    }

    /// The box's hardware: the screen `size` (`<width>x<height>`) at the Pixel 5's width in dp; a
    /// hardware keyboard, so the on-screen one doesn't cover what the driver taps; the system
    /// image's path, which avdmanager writes relative to /opt, as the command-line tools aren't in
    /// the cmdline-tools/latest layout it expects. The same each time.
    pub fn for_box(&mut self, size: &str) -> Result<()> {
        let (width, height) = size
            .split_once('x')
            .and_then(|(width, height)| {
                Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?))
            })
            .ok_or_else(|| Error::Failed(format!("a screen of {size}: <width>x<height>")))?;
        let sysdir = self.get(SYSDIR)?;
        let sysdir = sysdir
            .strip_prefix("android-sdk/")
            .unwrap_or(sysdir)
            .to_owned();
        if !sysdir.starts_with("system-images/") {
            return Err(Error::Failed(format!("config.ini's {SYSDIR} is {sysdir}")));
        }
        self.set(SYSDIR, &sysdir)?;
        self.set("hw.keyboard", "yes")?;
        self.set("hw.lcd.width", &width.to_string())?;
        self.set("hw.lcd.height", &height.to_string())?;
        self.set(
            "hw.lcd.density",
            &(width * PIXEL_5.1 / PIXEL_5.0).to_string(),
        )
    }
}

const SYSDIR: &str = "image.sysdir.1";

impl fmt::Display for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in &self.0 {
            match line {
                Line::Setting { key, value } => writeln!(formatter, "{key}={value}")?,
                Line::Other(other) => writeln!(formatter, "{other}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVDMANAGER: &str = "\
AvdId=box
hw.keyboard=no
hw.lcd.density=440
hw.lcd.height=2340
hw.lcd.width=1080
image.sysdir.1=android-sdk/system-images/android-35/default/x86_64/
";

    #[test]
    fn the_box_keeps_the_pixel_5s_width_in_dp() {
        let mut config = Config::parse(AVDMANAGER);
        config.for_box("720x1560").expect("the box's settings");
        assert_eq!(
            config.to_string(),
            "\
AvdId=box
hw.keyboard=yes
hw.lcd.density=293
hw.lcd.height=1560
hw.lcd.width=720
image.sysdir.1=system-images/android-35/default/x86_64/
"
        );
    }

    /// Every start sets them, on the config the last one wrote.
    #[test]
    fn setting_them_again_changes_nothing() {
        let mut config = Config::parse(AVDMANAGER);
        config.for_box("720x1560").expect("the first time");
        let once = config.to_string();
        let mut again = Config::parse(&once);
        again.for_box("720x1560").expect("the second time");
        assert_eq!(again.to_string(), once);
    }

    #[test]
    fn a_setting_the_avd_lacks_fails() {
        let mut config = Config::parse("AvdId=box\n");
        assert!(config.set("hw.keyboard", "yes").is_err());
        assert!(config.for_box("720x1560").is_err());
    }

    #[test]
    fn a_size_that_isnt_one_fails() {
        assert!(Config::parse(AVDMANAGER).for_box("720").is_err());
    }
}
