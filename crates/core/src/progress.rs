//! Module for the implementation of a progress bar.
//!
//! This is heavily influenced by the `cargo` implementation so
//! that it has the same appearance.

use crate::terminal::{Terminal, Verbosity};
use anyhow::Result;
use owo_colors::OwoColorize;
use std::io::{stderr, Write};
use std::time::{Duration, Instant};
use std::{cmp, fmt};
use unicode_width::UnicodeWidthChar;

fn is_ci() -> bool {
    std::env::var("CI").is_ok() || std::env::var("TF_BUILD").is_ok()
}

/// A progress bar implementation.
pub struct ProgressBar<'a> {
    state: Option<State<'a>>,
}

/// Indicates the style of information for displaying the amount of progress.
///
/// See also [`ProgressBar::print_now`] for displaying progress without a bar.
pub enum ProgressStyle {
    /// Displays progress as a percentage.
    ///
    /// Example: `Fetch [=====================>   ]  88.15%`
    ///
    /// This is good for large values like number of bytes downloaded.
    Percentage,
    /// Displays progress as a ratio.
    ///
    /// Example: `Building [===>                      ] 35/222`
    ///
    /// This is good for smaller values where the exact number is useful to see.
    Ratio,
    /// Does not display an exact value of how far along it is.
    ///
    /// Example: `Fetch [===========>                     ]`
    ///
    /// This is good for situations where the exact value is an approximation,
    /// and thus there isn't anything accurate to display to the user.
    Indeterminate,
}

struct Throttle {
    first: bool,
    last_update: Instant,
}

struct State<'a> {
    terminal: &'a Terminal,
    format: Format,
    name: String,
    done: bool,
    throttle: Throttle,
    last_line: Option<String>,
}

struct Format {
    style: ProgressStyle,
    max_width: usize,
    max_print: usize,
}

impl<'a> ProgressBar<'a> {
    /// Creates a new progress bar.
    ///
    /// The first parameter is the text displayed to the left of the bar, such
    /// as "Fetching".
    ///
    /// The progress bar is not displayed until explicitly updated with one if
    /// its methods.
    ///
    /// The progress bar may be created in a disabled state if the user has
    /// disabled progress display (such as with quiet verbosity).
    pub fn with_style(name: &str, style: ProgressStyle, terminal: &'a Terminal) -> Self {
        // report no progress when -q (for quiet) or TERM=dumb are set
        // or if running on Continuous Integration service like Travis where the
        // output logs get mangled.
        let dumb = match std::env::var("TERM") {
            Ok(term) => term == "dumb",
            Err(_) => false,
        };

        let verbosity = terminal.verbosity();
        if verbosity == Verbosity::Quiet || dumb || is_ci() {
            return Self { state: None };
        }

        Self::new_priv(name, style, terminal)
    }

    fn new_priv(name: &str, style: ProgressStyle, terminal: &'a Terminal) -> Self {
        let width = terminal.width();

        Self {
            state: width.map(|n| State {
                terminal,
                format: Format {
                    style,
                    max_width: n,
                    // 50 gives some space for text after the progress bar,
                    // even on narrow (e.g. 80 char) terminals.
                    max_print: 50,
                },
                name: name.to_string(),
                done: false,
                throttle: Throttle::new(),
                last_line: None,
            }),
        }
    }

    /// Disables the progress bar, ensuring it won't be displayed.
    pub fn disable(&mut self) {
        self.state = None;
    }

    /// Returns whether or not the progress bar is allowed to be displayed.
    pub fn is_enabled(&self) -> bool {
        self.state.is_some()
    }

    /// Creates a new `Progress` with the [`ProgressStyle::Percentage`] style.
    ///
    /// See [`ProgressBar::with_style`] for more information.
    pub fn new(name: &str, terminal: &'a Terminal) -> Self {
        Self::with_style(name, ProgressStyle::Percentage, terminal)
    }

    /// Updates the state of the progress bar.
    ///
    /// * `cur` should be how far along the progress is.
    /// * `max` is the maximum value for the progress bar.
    /// * `msg` is a small piece of text to display at the end of the progress
    ///   bar. It will be truncated with `...` if it does not fit on the
    ///   terminal.
    ///
    /// This may not actually update the display if `tick` is being called too
    /// quickly.
    pub fn tick(&mut self, cur: usize, max: usize, msg: &str) -> Result<()> {
        let s = match &mut self.state {
            Some(s) => s,
            None => return Ok(()),
        };

        // Don't update too often as it can cause excessive performance loss
        // just putting stuff onto the terminal. We also want to avoid
        // flickering by not drawing anything that goes away too quickly. As a
        // result we've got two branches here:
        //
        // 1. If we haven't drawn anything, we wait for a period of time to
        //    actually start drawing to the console. This ensures that
        //    short-lived operations don't flicker on the console. Currently
        //    there's a 500ms delay to when we first draw something.
        // 2. If we've drawn something, then we rate limit ourselves to only
        //    draw to the console every so often. Currently there's a 100ms
        //    delay between updates.
        if !s.throttle.allowed() {
            return Ok(());
        }

        s.tick(cur, max, msg)
    }

    /// Updates the state of the progress bar.
    ///
    /// This is the same as [`ProgressBar::tick`], but ignores rate throttling
    /// and forces the display to be updated immediately.
    ///
    /// This may be useful for situations where you know you aren't calling
    /// `tick` too fast, and accurate information is more important than
    /// limiting the console update rate.
    pub fn tick_now(&mut self, cur: usize, max: usize, msg: &str) -> Result<()> {
        match self.state {
            Some(ref mut s) => s.tick(cur, max, msg),
            None => Ok(()),
        }
    }

    /// Returns whether or not updates are currently being throttled.
    ///
    /// This can be useful if computing the values for calling the
    /// [`ProgressBar::tick`] function may require some expensive work.
    pub fn update_allowed(&mut self) -> bool {
        match &mut self.state {
            Some(s) => s.throttle.allowed(),
            None => false,
        }
    }

    /// Displays progress without a bar.
    ///
    /// The given `msg` is the text to display after the status message.
    ///
    /// Example: `Downloading 61 crates, remaining bytes: 28.0 MB`
    ///
    /// This does not have any rate limit throttling, so be careful about
    /// calling it too often.
    pub fn print_now(&mut self, msg: &str) -> Result<()> {
        match &mut self.state {
            Some(s) => s.print("", msg),
            None => Ok(()),
        }
    }

    /// Clears the progress bar from the console.
    pub fn clear(&mut self) {
        if let Some(ref mut s) = self.state {
            s.clear();
        }
    }
}

impl Throttle {
    fn new() -> Throttle {
        Throttle {
            first: true,
            last_update: Instant::now(),
        }
    }

    fn allowed(&mut self) -> bool {
        if self.first {
            let delay = Duration::from_millis(500);
            if self.last_update.elapsed() < delay {
                return false;
            }
        } else {
            let interval = Duration::from_millis(100);
            if self.last_update.elapsed() < interval {
                return false;
            }
        }
        self.update();
        true
    }

    fn update(&mut self) {
        self.first = false;
        self.last_update = Instant::now();
    }
}

impl<'a> State<'a> {
    fn tick(&mut self, cur: usize, max: usize, msg: &str) -> Result<()> {
        if self.done {
            return Ok(());
        }

        if max > 0 && cur == max {
            self.done = true;
        }

        // Write out a pretty header, then the progress bar itself, and then
        // return back to the beginning of the line for the next print.
        self.try_update_max_width();
        if let Some(pbar) = self.format.progress(cur, max) {
            self.print(&pbar, msg)?;
        }
        Ok(())
    }

    fn print(&mut self, prefix: &str, msg: &str) -> Result<()> {
        self.throttle.update();
        self.try_update_max_width();

        // make sure we have enough room for the header
        if self.format.max_width < 15 {
            return Ok(());
        }

        let mut line = prefix.to_string();
        self.format.render(&mut line, msg);
        while line.len() < self.format.max_width - 15 {
            line.push(' ');
        }

        let mut state = self.terminal.state_mut();

        // Only update if the line has changed.
        if !state.needs_clear || self.last_line.as_ref() != Some(&line) {
            let name_cyan = self.name.cyan();

            let status = if state.output.supports_color() {
                &name_cyan as &dyn fmt::Display
            } else {
                &self.name
            };

            state.output.print(status, None, true)?;
            write!(&mut stderr(), "{line}\r")?;
            self.last_line = Some(line);
            state.needs_clear = true;
        }

        Ok(())
    }

    fn clear(&mut self) {
        // No need to clear if the progress is not currently being displayed.
        if self.last_line.is_some() {
            self.terminal.state_mut().clear_stderr();
            self.last_line = None;
        }
    }

    fn try_update_max_width(&mut self) {
        if let Some(width) = self.terminal.width() {
            self.format.max_width = width;
        }
    }
}

impl Format {
    fn progress(&self, cur: usize, max: usize) -> Option<String> {
        assert!(cur <= max);
        // Render the percentage at the far right and then figure how long the
        // progress bar is
        let pct = (cur as f64) / (max as f64);
        let pct = if !pct.is_finite() { 0.0 } else { pct };
        let stats = match self.style {
            ProgressStyle::Percentage => format!(" {:6.02}%", pct * 100.0),
            ProgressStyle::Ratio => format!(" {}/{}", cur, max),
            ProgressStyle::Indeterminate => String::new(),
        };
        let extra_len = stats.len() + 2 /* [ and ] */ + 15 /* status header */;
        let display_width = match self.width().checked_sub(extra_len) {
            Some(n) => n,
            None => return None,
        };

        let mut string = String::with_capacity(self.max_width);
        string.push('[');
        let hashes = display_width as f64 * pct;
        let hashes = hashes as usize;

        // Draw the `===>`
        if hashes > 0 {
            for _ in 0..hashes - 1 {
                string.push('=');
            }
            if cur == max {
                string.push('=');
            } else {
                string.push('>');
            }
        }

        // Draw the empty space we have left to do
        for _ in 0..(display_width - hashes) {
            string.push(' ');
        }
        string.push(']');
        string.push_str(&stats);

        Some(string)
    }

    fn render(&self, string: &mut String, msg: &str) {
        let mut avail_msg_len = self.max_width - string.len() - 15;
        let mut ellipsis_pos = 0;
        if avail_msg_len <= 3 {
            return;
        }
        for c in msg.chars() {
            let display_width = c.width().unwrap_or(0);
            if avail_msg_len >= display_width {
                avail_msg_len -= display_width;
                string.push(c);
                if avail_msg_len >= 3 {
                    ellipsis_pos = string.len();
                }
            } else {
                string.truncate(ellipsis_pos);
                string.push_str("...");
                break;
            }
        }
    }

    #[cfg(test)]
    fn progress_status(&self, cur: usize, max: usize, msg: &str) -> Option<String> {
        let mut ret = self.progress(cur, max)?;
        self.render(&mut ret, msg);
        Some(ret)
    }

    fn width(&self) -> usize {
        cmp::min(self.max_width, self.max_print)
    }
}

impl<'a> Drop for State<'a> {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_progress_status() {
        let format = Format {
            style: ProgressStyle::Ratio,
            max_print: 40,
            max_width: 60,
        };
        assert_eq!(
            format.progress_status(0, 4, ""),
            Some("[                   ] 0/4".to_string())
        );
        assert_eq!(
            format.progress_status(1, 4, ""),
            Some("[===>               ] 1/4".to_string())
        );
        assert_eq!(
            format.progress_status(2, 4, ""),
            Some("[========>          ] 2/4".to_string())
        );
        assert_eq!(
            format.progress_status(3, 4, ""),
            Some("[=============>     ] 3/4".to_string())
        );
        assert_eq!(
            format.progress_status(4, 4, ""),
            Some("[===================] 4/4".to_string())
        );

        assert_eq!(
            format.progress_status(3999, 4000, ""),
            Some("[===========> ] 3999/4000".to_string())
        );
        assert_eq!(
            format.progress_status(4000, 4000, ""),
            Some("[=============] 4000/4000".to_string())
        );

        assert_eq!(
            format.progress_status(3, 4, ": short message"),
            Some("[=============>     ] 3/4: short message".to_string())
        );
        assert_eq!(
            format.progress_status(3, 4, ": msg thats just fit"),
            Some("[=============>     ] 3/4: msg thats just fit".to_string())
        );
        assert_eq!(
            format.progress_status(3, 4, ": msg that's just fit"),
            Some("[=============>     ] 3/4: msg that's just...".to_string())
        );

        // combining diacritics have width zero and thus can fit max_width.
        let zalgo_msg = "z̸̧̢̗͉̝̦͍̱ͧͦͨ̑̅̌ͥ́͢a̢ͬͨ̽ͯ̅̑ͥ͋̏̑ͫ̄͢͏̫̝̪̤͎̱̣͍̭̞̙̱͙͍̘̭͚l̶̡̛̥̝̰̭̹̯̯̞̪͇̱̦͙͔̘̼͇͓̈ͨ͗ͧ̓͒ͦ̀̇ͣ̈ͭ͊͛̃̑͒̿̕͜g̸̷̢̩̻̻͚̠͓̞̥͐ͩ͌̑ͥ̊̽͋͐̐͌͛̐̇̑ͨ́ͅo͙̳̣͔̰̠̜͕͕̞̦̙̭̜̯̹̬̻̓͑ͦ͋̈̉͌̃ͯ̀̂͠ͅ ̸̡͎̦̲̖̤̺̜̮̱̰̥͔̯̅̏ͬ̂ͨ̋̃̽̈́̾̔̇ͣ̚͜͜h̡ͫ̐̅̿̍̀͜҉̛͇̭̹̰̠͙̞ẽ̶̙̹̳̖͉͎̦͂̋̓ͮ̔ͬ̐̀͂̌͑̒͆̚͜͠ ͓͓̟͍̮̬̝̝̰͓͎̼̻ͦ͐̾̔͒̃̓͟͟c̮̦͍̺͈͚̯͕̄̒͐̂͊̊͗͊ͤͣ̀͘̕͝͞o̶͍͚͍̣̮͌ͦ̽̑ͩ̅ͮ̐̽̏͗́͂̅ͪ͠m̷̧͖̻͔̥̪̭͉͉̤̻͖̩̤͖̘ͦ̂͌̆̂ͦ̒͊ͯͬ͊̉̌ͬ͝͡e̵̹̣͍̜̺̤̤̯̫̹̠̮͎͙̯͚̰̼͗͐̀̒͂̉̀̚͝͞s̵̲͍͙͖̪͓͓̺̱̭̩̣͖̣ͤͤ͂̎̈͗͆ͨͪ̆̈͗͝͠";
        assert_eq!(
            format.progress_status(3, 4, zalgo_msg),
            Some("[=============>     ] 3/4".to_string() + zalgo_msg)
        );

        // some non-ASCII ellipsize test
        assert_eq!(
            format.progress_status(3, 4, "_123456789123456e\u{301}\u{301}8\u{301}90a"),
            Some("[=============>     ] 3/4_123456789123456e\u{301}\u{301}...".to_string())
        );
        assert_eq!(
            format.progress_status(3, 4, "：每個漢字佔據了兩個字元"),
            Some("[=============>     ] 3/4：每個漢字佔據了...".to_string())
        );
        assert_eq!(
            // handle breaking at middle of character
            format.progress_status(3, 4, "：-每個漢字佔據了兩個字元"),
            Some("[=============>     ] 3/4：-每個漢字佔據了...".to_string())
        );
    }

    #[test]
    fn test_progress_status_percentage() {
        let format = Format {
            style: ProgressStyle::Percentage,
            max_print: 40,
            max_width: 60,
        };
        assert_eq!(
            format.progress_status(0, 77, ""),
            Some("[               ]   0.00%".to_string())
        );
        assert_eq!(
            format.progress_status(1, 77, ""),
            Some("[               ]   1.30%".to_string())
        );
        assert_eq!(
            format.progress_status(76, 77, ""),
            Some("[=============> ]  98.70%".to_string())
        );
        assert_eq!(
            format.progress_status(77, 77, ""),
            Some("[===============] 100.00%".to_string())
        );
    }

    #[test]
    fn test_progress_status_too_short() {
        let format = Format {
            style: ProgressStyle::Percentage,
            max_print: 25,
            max_width: 25,
        };
        assert_eq!(
            format.progress_status(1, 1, ""),
            Some("[] 100.00%".to_string())
        );

        let format = Format {
            style: ProgressStyle::Percentage,
            max_print: 24,
            max_width: 24,
        };
        assert_eq!(format.progress_status(1, 1, ""), None);
    }
}
