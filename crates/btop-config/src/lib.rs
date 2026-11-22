//! Configuration (`btop.conf`) and `.theme` parsing with upstream `aristocratos/btop`
//! compatibility.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid line {line}: {message}")]
    Parse { line: usize, message: String },
    #[error("invalid value for '{key}': {value}")]
    InvalidValue { key: String, value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphSymbol {
    Braille,
    Block,
    Tty,
}

impl GraphSymbol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Braille => "braille",
            Self::Block => "block",
            Self::Tty => "tty",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub color_theme: String,
    pub theme_background: bool,
    pub truecolor: bool,
    pub force_tty: bool,
    pub rounded_corners: bool,
    pub terminal_sync: bool,
    pub vim_keys: bool,
    pub disable_mouse: bool,
    pub graph_symbol: GraphSymbol,
    pub shown_boxes: String,
    pub update_ms: u32,
    pub clock_format: String,
    pub proc_sorting: String,
    pub proc_reversed: bool,
    pub proc_tree: bool,
    pub proc_per_core: bool,
    pub proc_mem_bytes: bool,
    pub cpu_util_mode: String,
    pub freq_mode: String,
    /// Unknown or deferred upstream keys preserved verbatim when saving `btop.conf`.
    pub preserved_unknown: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            color_theme: "Default".to_owned(),
            theme_background: true,
            truecolor: true,
            force_tty: false,
            rounded_corners: true,
            terminal_sync: true,
            vim_keys: false,
            disable_mouse: false,
            graph_symbol: GraphSymbol::Braille,
            shown_boxes: "cpu mem net proc".to_owned(),
            update_ms: 2000,
            clock_format: "%X".to_owned(),
            proc_sorting: "cpu lazy".to_owned(),
            proc_reversed: false,
            proc_tree: false,
            proc_per_core: false,
            proc_mem_bytes: true,
            cpu_util_mode: "time".to_owned(),
            freq_mode: "average".to_owned(),
            preserved_unknown: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Renders a default `btop.conf` compatible with upstream `aristocratos/btop`.
    pub fn render_default_conf() -> String {
        let cfg = Self::default();
        format!(
            concat!(
                "#? Config file for btop-windows (English Windows 11 x64)\n\n",
                "#* Name of a .theme file or \"Default\".\n",
                "color_theme = \"{color_theme}\"\n\n",
                "#* Show theme background color; set False for terminal transparency.\n",
                "theme_background = {theme_background}\n\n",
                "#* Rounded corners on boxes.\n",
                "rounded_corners = {rounded_corners}\n\n",
                "#* Use DEC 2026 synchronized output sequences to prevent tearing.\n",
                "terminal_sync = {terminal_sync}\n\n",
                "#* Default graph symbol: \"braille\" or \"block\".\n",
                "graph_symbol = \"{graph_symbol}\"\n\n",
                "#* Shown panels: \"cpu mem net proc\" and \"gpu0\" through \"gpu5\".\n",
                "shown_boxes = \"{shown_boxes}\"\n\n",
                "#* Update interval in milliseconds (>= 100).\n",
                "update_ms = {update_ms}\n\n",
                "#* Clock format (strftime plus /host, /user, /uptime).\n",
                "clock_format = \"{clock_format}\"\n\n",
                "#* Process sort column.\n",
                "proc_sorting = \"{proc_sorting}\"\n",
                "proc_reversed = {proc_reversed}\n",
                "proc_tree = {proc_tree}\n\n",
                "#* Windows CPU utilization mode: \"time\" or \"utility\" (Task Manager).\n",
                "cpu_util_mode = \"{cpu_util_mode}\"\n\n",
                "#* CPU frequency aggregation: \"first\", \"range\", \"lowest\", \"highest\", or \"average\".\n",
                "freq_mode = \"{freq_mode}\"\n",
            ),
            color_theme = cfg.color_theme,
            theme_background = bool_title(cfg.theme_background),
            rounded_corners = bool_title(cfg.rounded_corners),
            terminal_sync = bool_title(cfg.terminal_sync),
            graph_symbol = cfg.graph_symbol.as_str(),
            shown_boxes = cfg.shown_boxes,
            update_ms = cfg.update_ms,
            clock_format = cfg.clock_format,
            proc_sorting = cfg.proc_sorting,
            proc_reversed = bool_title(cfg.proc_reversed),
            proc_tree = bool_title(cfg.proc_tree),
            cpu_util_mode = cfg.cpu_util_mode,
            freq_mode = cfg.freq_mode,
        )
    }

    /// Parses an upstream-style `key = value` config string while preserving unknown keys.
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        let mut cfg = Self::default();
        for (idx, raw_line) in input.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((raw_key, raw_val)) = line.split_once('=') else {
                return Err(ConfigError::Parse {
                    line: idx + 1,
                    message: "expected '='".to_owned(),
                });
            };
            let key = raw_key.trim();
            let val = unquote(raw_val.trim());
            match key {
                "color_theme" => cfg.color_theme = val,
                "theme_background" => cfg.theme_background = parse_bool(key, &val)?,
                "truecolor" => cfg.truecolor = parse_bool(key, &val)?,
                "force_tty" => cfg.force_tty = parse_bool(key, &val)?,
                "rounded_corners" => cfg.rounded_corners = parse_bool(key, &val)?,
                "terminal_sync" => cfg.terminal_sync = parse_bool(key, &val)?,
                "vim_keys" => cfg.vim_keys = parse_bool(key, &val)?,
                "disable_mouse" => cfg.disable_mouse = parse_bool(key, &val)?,
                "shown_boxes" => cfg.shown_boxes = val,
                "clock_format" => cfg.clock_format = val,
                "proc_sorting" => cfg.proc_sorting = val,
                "proc_reversed" => cfg.proc_reversed = parse_bool(key, &val)?,
                "proc_tree" => cfg.proc_tree = parse_bool(key, &val)?,
                "proc_per_core" => cfg.proc_per_core = parse_bool(key, &val)?,
                "proc_mem_bytes" => cfg.proc_mem_bytes = parse_bool(key, &val)?,
                "cpu_util_mode" => cfg.cpu_util_mode = val,
                "freq_mode" => cfg.freq_mode = val,
                "graph_symbol" => {
                    cfg.graph_symbol = match val.as_str() {
                        "braille" => GraphSymbol::Braille,
                        "block" => GraphSymbol::Block,
                        "tty" => GraphSymbol::Tty,
                        _ => {
                            return Err(ConfigError::InvalidValue {
                                key: key.to_owned(),
                                value: val,
                            });
                        }
                    };
                }
                "update_ms" => {
                    let parsed: u32 = val.parse().map_err(|_| ConfigError::InvalidValue {
                        key: key.to_owned(),
                        value: val.clone(),
                    })?;
                    cfg.update_ms = parsed.max(100);
                }
                other => {
                    cfg.preserved_unknown.insert(other.to_owned(), val);
                }
            }
        }
        Ok(cfg)
    }
}

fn bool_title(v: bool) -> &'static str {
    if v { "True" } else { "False" }
}

fn unquote(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_owned()
    } else {
        s.to_owned()
    }
}

fn parse_bool(key: &str, val: &str) -> Result<bool, ConfigError> {
    match val.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(ConfigError::InvalidValue {
            key: key.to_owned(),
            value: val.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_conf_roundtrips_and_preserves_unix_keys() {
        let mut text = Config::render_default_conf();
        text.push_str("\nuse_fstab = True\nzfs_arc_cached = True\n");
        let parsed = Config::parse(&text).expect("parse default conf");
        assert_eq!(parsed.update_ms, 2000);
        assert_eq!(parsed.graph_symbol, GraphSymbol::Braille);
        assert_eq!(
            parsed
                .preserved_unknown
                .get("use_fstab")
                .map(String::as_str),
            Some("True")
        );
    }

    #[test]
    fn clamps_update_ms_floor() {
        let parsed = Config::parse("update_ms = 25\n").expect("parse");
        assert_eq!(parsed.update_ms, 100);
    }
}
