//! tmux style-string grammar (`fg=colour208,bg=#1e1e2e,bold,underscore`) layered
//! onto [`crate::grid::Style`].
//!
//! Pure module: no I/O, `std` only.
//!
//! [`parse_style`] parses one comma-separated tmux style string into a
//! [`PartialStyle`] — a set of *explicit* overrides (colors and attribute
//! set/clear flags). Only components actually named in the input are
//! recorded; [`PartialStyle::apply_to`] then layers those explicit overrides
//! onto a base [`crate::grid::Style`], leaving everything unmentioned
//! untouched. [`PartialStyle::merge`] composes two `PartialStyle`s for
//! layered options (e.g. `window-status-current-style` over `status-style`).

use crate::grid::{Color, Style};

/// A parsed tmux style string: only the fields actually mentioned by the
/// input are `Some`/set — everything else is left as "unmentioned" so it can
/// be layered onto a base style without clobbering it. Opaque: constructed
/// only via [`parse_style`], inspected only via [`PartialStyle::apply_to`]
/// and [`PartialStyle::merge`].
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct PartialStyle {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: Option<bool>,
    dim: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    reverse: Option<bool>,
}

impl PartialStyle {
    /// Layer this style's explicit overrides onto `base`. Colors: `Some(c)`
    /// replaces the base's color; unmentioned stays as `base`'s. Attributes:
    /// `Some(true)`/`Some(false)` force the base's flag on/off (a `no<attr>`
    /// clear is just as "explicit" as setting it); unmentioned stays as
    /// `base`'s.
    pub fn apply_to(&self, base: Style) -> Style {
        let mut s = base;
        if let Some(fg) = self.fg {
            s.fg = fg;
        }
        if let Some(bg) = self.bg {
            s.bg = bg;
        }
        if let Some(b) = self.bold {
            s.bold = b;
        }
        if let Some(d) = self.dim {
            s.dim = d;
        }
        if let Some(i) = self.italic {
            s.italic = i;
        }
        if let Some(u) = self.underline {
            s.underline = u;
        }
        if let Some(r) = self.reverse {
            s.reverse = r;
        }
        s
    }

    /// Compose two partial styles for layering (e.g.
    /// `window-status-current-style` over `window-status-style`): every
    /// field explicit in `over` wins; fields `over` leaves unmentioned fall
    /// back to this style's own value (which may itself be unmentioned).
    pub fn merge(&self, over: &PartialStyle) -> PartialStyle {
        PartialStyle {
            fg: over.fg.or(self.fg),
            bg: over.bg.or(self.bg),
            bold: over.bold.or(self.bold),
            dim: over.dim.or(self.dim),
            italic: over.italic.or(self.italic),
            underline: over.underline.or(self.underline),
            reverse: over.reverse.or(self.reverse),
        }
    }
}

/// Parse a tmux style string (e.g. `"fg=colour208,bg=#1e1e2e,bold,underscore"`)
/// into a [`PartialStyle`]. The whole input is trimmed of surrounding
/// whitespace first; an empty result is `Ok` with a no-op `PartialStyle`
/// (nothing mentioned). Otherwise the (untrimmed-per-component) string is
/// split on `,`; each component must be one of:
///
/// - `fg=<color>` / `bg=<color>` — see [`parse_color`] for the color grammar.
/// - `none` / `noattr` — resets accumulated attribute state (bold/dim/
///   italic/underline/reverse) back to "unmentioned" for this style. Per
///   tmux, this does NOT touch `fg`/`bg` — those are left as already parsed.
/// - `bold` / `nobold`, `dim` / `nodim`, `reverse` / `noreverse` — set/clear.
/// - `italics` or `italic` / `noitalics` or `noitalic` — set/clear (tmux's
///   canonical word is `italics`; `italic` is accepted as a synonym).
/// - `underscore` or `underline` / `nounderscore` or `nounderline` — set/clear
///   (tmux's canonical word is `underscore`; `underline` is accepted as a
///   synonym).
/// - `blink`/`noblink`, `strikethrough`/`nostrikethrough`, and
///   `double-underscore`, `curly-underscore`, `dotted-underscore`,
///   `dashed-underscore` plus their `no*` forms — accepted but inert: they
///   parse successfully and are otherwise no-ops (no field in `grid::Style`
///   represents them).
///
/// Matching is **case-insensitive** throughout (`FG=Red`, `BOLD`, `NONE`),
/// mirroring tmux's `strcasecmp`-based style and color parsing.
///
/// Any other component (unknown attribute word, malformed color, an empty
/// component from a stray/leading/trailing/doubled comma) is a parse
/// failure; the whole call then fails with `Err("bad style: <input>")`
/// where `<input>` is the exact original (untrimmed, original-case) input
/// string.
pub fn parse_style(input: &str) -> Result<PartialStyle, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(PartialStyle::default());
    }
    let mut style = PartialStyle::default();
    for part in trimmed.split(',') {
        // tmux matches style keys, attribute words, and color names via
        // strcasecmp — lowercase the whole component before matching. (Hex
        // digits after `#` are unaffected: they are case-insensitive anyway.)
        // The error string below still echoes the ORIGINAL input case.
        apply_component(&mut style, &part.to_ascii_lowercase())
            .map_err(|()| format!("bad style: {input}"))?;
    }
    Ok(style)
}

fn apply_component(style: &mut PartialStyle, part: &str) -> Result<(), ()> {
    match part {
        "none" | "noattr" => {
            style.bold = None;
            style.dim = None;
            style.italic = None;
            style.underline = None;
            style.reverse = None;
            return Ok(());
        }
        "bold" => {
            style.bold = Some(true);
            return Ok(());
        }
        "nobold" => {
            style.bold = Some(false);
            return Ok(());
        }
        "dim" => {
            style.dim = Some(true);
            return Ok(());
        }
        "nodim" => {
            style.dim = Some(false);
            return Ok(());
        }
        "italics" | "italic" => {
            style.italic = Some(true);
            return Ok(());
        }
        "noitalics" | "noitalic" => {
            style.italic = Some(false);
            return Ok(());
        }
        "underscore" | "underline" => {
            style.underline = Some(true);
            return Ok(());
        }
        "nounderscore" | "nounderline" => {
            style.underline = Some(false);
            return Ok(());
        }
        "reverse" => {
            style.reverse = Some(true);
            return Ok(());
        }
        "noreverse" => {
            style.reverse = Some(false);
            return Ok(());
        }
        "blink" | "noblink" | "strikethrough" | "nostrikethrough" | "double-underscore"
        | "nodouble-underscore" | "curly-underscore" | "nocurly-underscore"
        | "dotted-underscore" | "nodotted-underscore" | "dashed-underscore"
        | "nodashed-underscore" => {
            return Ok(());
        }
        _ => {}
    }
    if let Some(rest) = part.strip_prefix("fg=") {
        style.fg = Some(parse_color(rest)?);
        return Ok(());
    }
    if let Some(rest) = part.strip_prefix("bg=") {
        style.bg = Some(parse_color(rest)?);
        return Ok(());
    }
    Err(())
}

/// Parse one tmux color token: `default`; a named ANSI color (`black` `red`
/// `green` `yellow` `blue` `magenta` `cyan` `white`, indices 0-7) or its
/// `bright<name>` variant (indices 8-15); `colour<n>` / `color<n>` for `n` in
/// `0..=255` (`colour256`+ is out of range: `Err`); or `#rrggbb` hex (exactly
/// 6 hex digits after the `#`; any other length or a non-hex digit is `Err`).
/// The caller has already lowercased the token, making every form
/// case-insensitive (tmux `colour_fromstring` uses `strcasecmp`).
fn parse_color(s: &str) -> Result<Color, ()> {
    if s == "default" {
        return Ok(Color::Default);
    }
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 || !hex.is_ascii() {
            return Err(());
        }
        let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ())?;
        let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ())?;
        let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ())?;
        return Ok(Color::Rgb(r, g, b));
    }
    if let Some(idx) = named_color_index(s) {
        return Ok(Color::Idx(idx));
    }
    for prefix in ["colour", "color"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            let n: u8 = rest.parse().map_err(|_| ())?;
            return Ok(Color::Idx(n));
        }
    }
    Err(())
}

fn named_color_index(s: &str) -> Option<u8> {
    Some(match s {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "white" => 7,
        "brightblack" => 8,
        "brightred" => 9,
        "brightgreen" => 10,
        "brightyellow" => 11,
        "brightblue" => 12,
        "brightmagenta" => 13,
        "brightcyan" => 14,
        "brightwhite" => 15,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- colors ----

    #[test]
    fn named_colors() {
        assert_eq!(
            parse_style("fg=red").unwrap().apply_to(Style::default()).fg,
            Color::Idx(1)
        );
        assert_eq!(
            parse_style("fg=brightred")
                .unwrap()
                .apply_to(Style::default())
                .fg,
            Color::Idx(9)
        );
        assert_eq!(
            parse_style("bg=black")
                .unwrap()
                .apply_to(Style::default())
                .bg,
            Color::Idx(0)
        );
        assert_eq!(
            parse_style("bg=brightwhite")
                .unwrap()
                .apply_to(Style::default())
                .bg,
            Color::Idx(15)
        );
    }

    #[test]
    fn colour_indexed() {
        assert_eq!(
            parse_style("fg=colour208")
                .unwrap()
                .apply_to(Style::default())
                .fg,
            Color::Idx(208)
        );
        assert_eq!(
            parse_style("fg=color208")
                .unwrap()
                .apply_to(Style::default())
                .fg,
            Color::Idx(208)
        );
        assert_eq!(
            parse_style("fg=colour255")
                .unwrap()
                .apply_to(Style::default())
                .fg,
            Color::Idx(255)
        );
        assert!(parse_style("fg=colour256").is_err());
    }

    #[test]
    fn hex_rgb() {
        assert_eq!(
            parse_style("bg=#1e1e2e")
                .unwrap()
                .apply_to(Style::default())
                .bg,
            Color::Rgb(0x1e, 0x1e, 0x2e)
        );
        // case-insensitive
        assert_eq!(
            parse_style("bg=#1E1E2E")
                .unwrap()
                .apply_to(Style::default())
                .bg,
            Color::Rgb(0x1e, 0x1e, 0x2e)
        );
        assert!(parse_style("bg=#1e1e2").is_err()); // too short
        assert!(parse_style("bg=#1e1e2ef").is_err()); // too long
        assert!(parse_style("bg=#gggggg").is_err()); // not hex
    }

    #[test]
    fn default_color_resets() {
        let base = Style {
            fg: Color::Idx(2),
            ..Style::default()
        };
        let applied = parse_style("fg=default").unwrap().apply_to(base);
        assert_eq!(applied.fg, Color::Default);
    }

    // ---- attributes ----

    #[test]
    fn attrs_set() {
        let s = parse_style("bold,underscore,reverse,dim,italics").unwrap();
        let applied = s.apply_to(Style::default());
        assert!(applied.bold);
        assert!(applied.underline);
        assert!(applied.reverse);
        assert!(applied.dim);
        assert!(applied.italic);
    }

    #[test]
    fn attr_synonyms() {
        let s = parse_style("italic,underline").unwrap();
        let applied = s.apply_to(Style::default());
        assert!(applied.italic);
        assert!(applied.underline);
    }

    #[test]
    fn attrs_clear() {
        let base = Style {
            bold: true,
            dim: true,
            italic: true,
            underline: true,
            reverse: true,
            ..Style::default()
        };
        let cleared_one = parse_style("nobold").unwrap().apply_to(base);
        assert!(!cleared_one.bold);
        assert!(cleared_one.dim); // untouched: still set from base

        // `none` resets accumulated attr state (set-or-cleared) — a
        // subsequent `bold` after `none` still takes effect fresh, but with
        // nothing after `none`, applying onto a set-everything base leaves
        // all attrs as the base had them (nothing is "mentioned" anymore).
        let reset = parse_style("bold,none").unwrap().apply_to(base);
        assert_eq!(reset, base);
    }

    #[test]
    fn accepted_ignored() {
        let s = parse_style("blink,strikethrough,double-underscore,dashed-underscore").unwrap();
        assert_eq!(s.apply_to(Style::default()), Style::default());
        // negated forms of the inert words are equally accepted-and-inert
        let n = parse_style(
            "noblink,nostrikethrough,nodouble-underscore,nocurly-underscore,\
             nodotted-underscore,nodashed-underscore",
        )
        .unwrap();
        assert_eq!(n.apply_to(Style::default()), Style::default());
    }

    #[test]
    fn color_names_case_insensitive() {
        // tmux matches style keys, attribute words, and color names via
        // strcasecmp — any case mix is valid tmux config.
        assert_eq!(
            parse_style("fg=Red").unwrap().apply_to(Style::default()).fg,
            Color::Idx(1)
        );
        assert_eq!(
            parse_style("fg=RED").unwrap().apply_to(Style::default()).fg,
            Color::Idx(1)
        );
        assert_eq!(
            parse_style("FG=red").unwrap().apply_to(Style::default()).fg,
            Color::Idx(1)
        );
        assert_eq!(
            parse_style("Bg=BrightRed")
                .unwrap()
                .apply_to(Style::default())
                .bg,
            Color::Idx(9)
        );
        assert_eq!(
            parse_style("fg=Colour208")
                .unwrap()
                .apply_to(Style::default())
                .fg,
            Color::Idx(208)
        );
        let base = Style {
            fg: Color::Idx(2),
            bold: true,
            ..Style::default()
        };
        assert_eq!(
            parse_style("fg=DEFAULT").unwrap().apply_to(base).fg,
            Color::Default
        );
        assert!(parse_style("BOLD").unwrap().apply_to(Style::default()).bold);
        assert!(!parse_style("NoBold").unwrap().apply_to(base).bold);
        assert_eq!(parse_style("bold,NONE").unwrap().apply_to(base), base);
        // the error string still echoes the ORIGINAL case of the input
        assert_eq!(parse_style("FG=zzz"), Err("bad style: FG=zzz".to_string()));
    }

    // ---- layering ----

    #[test]
    fn apply_layers_over_base() {
        let base = Style {
            fg: Color::Idx(2), // green
            bg: Color::Idx(0), // black
            ..Style::default()
        };
        let applied = parse_style("fg=red,bold").unwrap().apply_to(base);
        assert_eq!(applied.fg, Color::Idx(1));
        assert_eq!(applied.bg, Color::Idx(0)); // unmentioned: stays base's
        assert!(applied.bold);
    }

    #[test]
    fn merge_precedence() {
        let under = parse_style("fg=red,bold").unwrap();
        let over = parse_style("fg=blue").unwrap();
        let merged = under.merge(&over);
        let applied = merged.apply_to(Style::default());
        assert_eq!(applied.fg, Color::Idx(4)); // over wins
        assert!(applied.bold); // over left unmentioned: falls back to under
    }

    // ---- errors ----

    #[test]
    fn bad_style_err_string() {
        assert_eq!(parse_style("fg=zzz"), Err("bad style: fg=zzz".to_string()));
        assert_eq!(
            parse_style("notanattr"),
            Err("bad style: notanattr".to_string())
        );
        assert_eq!(
            parse_style("fg=red,,bold"),
            Err("bad style: fg=red,,bold".to_string())
        );
    }

    #[test]
    fn empty_string_ok_noop() {
        let base = Style {
            fg: Color::Idx(3),
            bold: true,
            ..Style::default()
        };
        assert_eq!(parse_style("").unwrap().apply_to(base), base);
        assert_eq!(parse_style("   ").unwrap().apply_to(base), base);
    }
}
