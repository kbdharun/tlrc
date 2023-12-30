use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

use yansi::Color::Green;
use yansi::{Paint, Style};

use crate::config::Config;
use crate::error::{Error, ErrorKind, Result};
use crate::util::{warnln, PagePathExt};

const TITLE: &str = "# ";
const DESC: &str = "> ";
const BULLET: &str = "- ";
const EXAMPLE: char = '`';

/// Highlight a substring between `start` and `end` inside `s` and return a new `String`.
fn highlight(start: &str, end: &str, s: &str, style_normal: Style, style_hl: Style) -> String {
    let split: Vec<&str> = s.split(start).collect();
    // Highlight beginning not found.
    if split.len() == 1 {
        return style_normal.paint(s).to_string();
    }

    let mut buf = String::new();

    if start == end {
        for (i, part) in split.into_iter().enumerate() {
            // Only odd indexes contain the part to be highlighted.
            // "aa `bb` cc `dd` ee"
            // 0: "aa "
            // 1: "bb"      (highlighted)
            // 2: " cc "
            // 3: "dd"      (highlighted)
            // 4: " ee"
            if i % 2 == 0 {
                buf += &style_normal.paint(part).to_string();
            } else {
                buf += &style_hl.paint(part).to_string();
            }
        }

        return buf;
    }

    for part in split {
        if part.contains(end) {
            // The first part of the second split contains the part to be highlighted.

            if end == ">" {
                // "More information: <https://example.com>."
                // 0: "More information: " => does not match
                // 1: "s://example.com>."  => 0: "s://example.com" (highlighted)
                //                            1: ">."
                let part_split = part.split_once('>').unwrap();

                // "<http" is used to detect URLs. It must be added back.
                let hl = format!("http{}", part_split.0);
                buf += &style_hl.paint(hl).to_string();
                buf += &style_normal.paint(part_split.1).to_string();
            } else {
                // "aa bb {{cc}} {{dd}} ee"
                // 0: "aa bb "   => does not match
                // 1: "cc}} "    => 0: "cc"    (highlighted)
                //                  1: "}}"
                // 2: "dd}} ee"  => 0: "dd"    (highlighted)
                //                  1: "}} ee"

                // This is required for special cases with three closing curly braces ("}}}").
                // The first brace is inside the placeholder, and the last two mark the end of it.
                let idx = part.rmatch_indices(end).last().unwrap().0;
                let part_spl = part.split_at(idx);

                buf += &style_hl.paint(part_spl.0).to_string();
                buf += &style_normal.paint(&part_spl.1[2..]).to_string();
            }
        } else {
            // Highlight ending not found.
            buf += &style_normal.paint(part).to_string();
        }
    }

    buf
}

struct RenderStyles {
    title: Style,
    desc: Style,
    bullet: Style,
    example: Style,
    url: Style,
    inline_code: Style,
    placeholder: Style,
}

pub struct PageRenderer<'a> {
    /// Path to the page.
    path: &'a Path,
    /// A BufReader containing the page.
    reader: BufReader<File>,
    /// A buffered handle to standard output.
    stdout: BufWriter<io::StdoutLock<'static>>,
    /// The line of the page that is currently being worked with.
    current_line: String,
    /// The line number of the current line.
    lnum: usize,
    /// Style configuration.
    style: RenderStyles,
    /// Other options.
    cfg: &'a Config,
}

impl<'a> PageRenderer<'a> {
    /// Print or render the page according to the provided config.
    pub fn print(path: &'a Path, cfg: &'a Config) -> Result<()> {
        let mut page = File::open(path)
            .map_err(|e| Error::new(format!("'{}': {e}", path.display())).kind(ErrorKind::Io))?;

        if cfg.output.raw_markdown {
            io::copy(&mut page, &mut io::stdout()).map_err(|e| {
                Error::new(format!("'{}': {e}", path.display())).kind(ErrorKind::Io)
            })?;
            return Ok(());
        }

        Self {
            path,
            reader: BufReader::new(page),
            stdout: BufWriter::new(io::stdout().lock()),
            current_line: String::new(),
            lnum: 0,
            style: RenderStyles {
                title: cfg.style.title.into(),
                desc: cfg.style.description.into(),
                bullet: cfg.style.bullet.into(),
                example: cfg.style.example.into(),
                url: cfg.style.url.into(),
                inline_code: cfg.style.inline_code.into(),
                placeholder: cfg.style.placeholder.into(),
            },
            cfg,
        }
        .render()
    }

    /// Print the first page that was found and warnings for every other page.
    pub fn print_cache_result(paths: &'a [PathBuf], cfg: &'a Config) -> Result<()> {
        if !crate::QUIET.load(Relaxed) {
            if let Some(others) = paths.get(1..) {
                if !others.is_empty() {
                    warnln!("{} page(s) found for other platforms:", others.len());
                }

                let mut stderr = io::stderr().lock();
                for (i, path) in others.iter().enumerate() {
                    // The path always ends with the page file, and its parent is always the
                    // platform directory. This is safe to unwrap.
                    let name = path.page_name().unwrap();
                    let platform = path.page_platform().unwrap();
                    writeln!(
                        stderr,
                        "{} '{platform}' (tldr --platform {platform} {name})",
                        Paint::new(format!("{}.", i + 1)).fg(Green).bold()
                    )?;
                }
            }
        }

        // This is safe to unwrap - errors would have already been catched in run().
        let first = paths.first().unwrap();
        Self::print(first, cfg)
    }

    /// Load the next line into the line buffer.
    fn next_line(&mut self) -> Result<usize> {
        self.current_line.clear();
        self.lnum += 1;
        self.reader
            .read_line(&mut self.current_line)
            .map_err(|e| Error::new(format!("'{}': {e}", self.path.display())))
    }

    /// Write the current line to the page buffer as a title.
    fn add_title(&mut self) -> Result<()> {
        if !self.cfg.output.show_title {
            return Ok(());
        }
        if !self.cfg.output.compact {
            writeln!(self.stdout)?;
        }

        let line = self.current_line.strip_prefix(TITLE).unwrap();
        let title = if self.cfg.output.platform_title {
            if let Some(platform) = self.path.page_platform() {
                Cow::Owned(format!("{platform}/{line}"))
            } else {
                Cow::Borrowed(line)
            }
        } else {
            Cow::Borrowed(line)
        };

        write!(
            self.stdout,
            "{}{}",
            " ".repeat(self.cfg.indent.title),
            self.style.title.paint(title)
        )?;

        Ok(())
    }

    /// Write the current line to the page buffer as a description.
    fn add_desc(&mut self) -> Result<()> {
        write!(
            self.stdout,
            "{}{}",
            " ".repeat(self.cfg.indent.description),
            highlight(
                "`",
                "`",
                &highlight(
                    "<http",
                    ">",
                    self.current_line.strip_prefix(DESC).unwrap(),
                    self.style.desc,
                    self.style.url,
                ),
                self.style.desc,
                self.style.inline_code,
            )
        )?;

        Ok(())
    }

    /// Write the current line to the page buffer as a bullet point.
    fn add_bullet(&mut self) -> Result<()> {
        let line = if self.cfg.output.show_hyphens {
            self.current_line
                .replace_range(..2, &self.cfg.output.example_prefix);
            &self.current_line
        } else {
            self.current_line.strip_prefix(BULLET).unwrap()
        };

        write!(
            self.stdout,
            "{}{}",
            " ".repeat(self.cfg.indent.bullet),
            highlight(
                "`",
                "`",
                &highlight("<http", ">", line, self.style.bullet, self.style.url),
                self.style.bullet,
                self.style.inline_code,
            )
        )?;

        Ok(())
    }

    /// Write the current line to the page buffer as an example.
    fn add_example(&mut self) -> Result<()> {
        // Add spaces around escaped curly braces in order not to
        // interpret them as a placeholder (e.g. in "\{\{{{ }}\}\}").
        self.current_line = self
            .current_line
            .replace("\\{\\{", " \\{\\{ ")
            .replace("\\}\\}", " \\}\\} ");

        writeln!(
            self.stdout,
            "{}{}",
            " ".repeat(self.cfg.indent.example),
            highlight(
                "{{",
                "}}",
                self.current_line
                    .strip_prefix(EXAMPLE)
                    .unwrap()
                    .trim_end()
                    .strip_suffix('`')
                    .ok_or_else(|| {
                        Error::parse_page(self.path, self.lnum, &self.current_line)
                            .describe("\nEvery line with an example must end with a backtick '`'.")
                    })?,
                self.style.example,
                self.style.placeholder,
            )
            // Remove the extra spaces and backslashes.
            .replace(" \\{\\{ ", "{{")
            .replace(" \\}\\} ", "}}")
        )?;

        Ok(())
    }

    /// Write a newline to the page buffer if compact mode is not turned on.
    fn add_newline(&mut self) -> Result<()> {
        if !self.cfg.output.compact {
            writeln!(self.stdout)?;
        }

        Ok(())
    }

    /// Render the page to standard output.
    fn render(&mut self) -> Result<()> {
        while self.next_line()? != 0 {
            if self.current_line.starts_with(TITLE) {
                self.add_title()?;
            } else if self.current_line.starts_with(DESC) {
                self.add_desc()?;
            } else if self.current_line.starts_with(BULLET) {
                self.add_bullet()?;
            } else if self.current_line.starts_with(EXAMPLE) {
                self.add_example()?;
            } else if self.current_line.chars().all(char::is_whitespace) {
                self.add_newline()?;
            } else {
                return Err(
                    Error::parse_page(self.path, self.lnum, &self.current_line).describe(
                        "\nEvery non-empty line must begin with either '# ', '> ', '- ' or '`'.",
                    ),
                );
            }
        }

        self.add_newline()?;
        Ok(self.stdout.flush()?)
    }
}
