//! Color themes. The default `editorial` theme is the identity of the
//! product: byte-identical to the original hardcoded palette. Additional
//! themes load from TOML files with optional dark/light variants.

use haarness_opentui::Color;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Page background used to clear each frame.
    pub bg: Color,
    /// Raised background for menus, code, and panels.
    pub surface: Color,
    /// Solid surface reserved for menus, dialogs, and other overlays. In
    /// terminal-background mode ordinary surfaces become transparent while
    /// overlays retain this color so controls stay legible.
    pub overlay: Color,
    /// Background of the selected menu row.
    pub selected: Color,
    /// Primary body text.
    pub text: Color,
    /// Secondary, de-emphasized text.
    pub muted: Color,
    /// Brand color: headers, assistant label, highlights.
    pub primary: Color,
    /// Warm accent: tool activity, attention.
    pub accent: Color,
    /// The user's own messages.
    pub user: Color,
    /// Errors and failures.
    pub danger: Color,
    pub syntax: SyntaxTheme,
    pub diff: DiffTheme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxTheme {
    pub keyword: Color,
    pub string: Color,
    pub comment: Color,
    pub constant: Color,
    pub function: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffTheme {
    pub added: Color,
    pub added_bg: Color,
    pub removed: Color,
    pub removed_bg: Color,
}

impl Theme {
    /// Use the terminal behind ordinary application surfaces, matching
    /// Haarness's terminal background mode. Selection, diff emphasis, and
    /// overlays remain solid because they convey state or cover other text.
    #[must_use]
    pub const fn using_terminal_background(mut self) -> Self {
        self.surface = Color::TRANSPARENT;
        self
    }

    /// Completed work: the diff's added tone, so "done" reads as growth in
    /// every palette instead of borrowing the user's own colour.
    #[must_use]
    pub const fn success(&self) -> Color {
        self.diff.added
    }

    /// Rule tone for the composer hairline and other structural edges: a
    /// step between the page and its quietest text, derived so every theme
    /// (including the embedded collection) gets one without a new palette key.
    #[must_use]
    pub fn hairline(&self) -> Color {
        blend(self.bg, self.muted, 0.45)
    }

    /// Move `amount` of the way from `from` toward `to`. The review viewer
    /// derives its cursor and comment tints from the palette this way so
    /// every theme — including user TOML — gets them without a new key.
    #[must_use]
    pub fn mix(from: Color, to: Color, amount: f64) -> Color {
        blend(from, to, amount)
    }

    /// The breathing activity rule's colour at `amount` (0..=1).
    ///
    /// Two stages rather than one blend: the bottom of the cycle pulls `color`
    /// back toward the page, and the top pushes it *past* `color` toward the
    /// page's text. A full-width line held at full strength would shout, but a
    /// line that only ever fades between the page and the primary has nowhere
    /// bright to go — the crest needs to outrun the theme's own colour to read
    /// as a beat rather than a slow fade.
    #[must_use]
    pub fn pulse(&self, color: Color, amount: f64) -> Color {
        const CREST: f64 = 0.85;
        const OVERSHOOT: f64 = 0.30;
        let amount = amount.clamp(0.0, 1.0);
        if amount <= CREST {
            blend(self.bg, color, 0.30 + (amount / CREST) * 0.70)
        } else {
            blend(
                color,
                self.text,
                (amount - CREST) / (1.0 - CREST) * OVERSHOOT,
            )
        }
    }

    #[must_use]
    pub const fn editorial_dark() -> Self {
        Self {
            bg: Color::rgb(15, 14, 13),
            surface: Color::rgb(27, 25, 23),
            overlay: Color::rgb(27, 25, 23),
            selected: Color::rgb(37, 57, 52),
            text: Color::rgb(231, 225, 213),
            muted: Color::rgb(126, 120, 111),
            primary: Color::rgb(62, 184, 166),
            accent: Color::rgb(211, 133, 82),
            user: Color::rgb(166, 205, 194),
            danger: Color::rgb(224, 104, 99),
            syntax: SyntaxTheme {
                keyword: Color::rgb(62, 184, 166),
                string: Color::rgb(166, 205, 194),
                comment: Color::rgb(126, 120, 111),
                constant: Color::rgb(211, 133, 82),
                function: Color::rgb(231, 225, 213),
            },
            diff: DiffTheme {
                added: Color::rgb(143, 191, 127),
                added_bg: Color::rgb(24, 34, 24),
                removed: Color::rgb(224, 104, 99),
                removed_bg: Color::rgb(42, 24, 24),
            },
        }
    }

    /// A restrained light companion to the editorial dark default.
    #[must_use]
    pub const fn editorial_light() -> Self {
        Self {
            bg: Color::rgb(247, 244, 238),
            surface: Color::rgb(234, 229, 219),
            overlay: Color::rgb(234, 229, 219),
            selected: Color::rgb(205, 226, 219),
            text: Color::rgb(43, 40, 36),
            muted: Color::rgb(125, 119, 109),
            primary: Color::rgb(16, 122, 108),
            accent: Color::rgb(164, 88, 41),
            user: Color::rgb(52, 100, 87),
            danger: Color::rgb(176, 58, 53),
            syntax: SyntaxTheme {
                keyword: Color::rgb(16, 122, 108),
                string: Color::rgb(52, 100, 87),
                comment: Color::rgb(125, 119, 109),
                constant: Color::rgb(164, 88, 41),
                function: Color::rgb(43, 40, 36),
            },
            diff: DiffTheme {
                added: Color::rgb(38, 111, 54),
                added_bg: Color::rgb(224, 238, 224),
                removed: Color::rgb(176, 58, 53),
                removed_bg: Color::rgb(243, 224, 222),
            },
        }
    }
}

/// Linear mix from `from` toward `to`. Terminal cells cannot fake opacity,
/// so every soft tone in the interface is resolved to a concrete color here.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=255 by construction
fn blend(from: Color, to: Color, amount: f64) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let (from, to) = (from.components(), to.components());
    let channel = |index: usize| {
        let base = f64::from(from[index]);
        (base + (f64::from(to[index]) - base) * amount)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::rgb(channel(0), channel(1), channel(2))
}

impl Default for Theme {
    fn default() -> Self {
        Self::editorial_dark()
    }
}

/// A named theme with its available variants.
#[derive(Clone, Debug)]
pub struct ThemeEntry {
    pub name: String,
    pub dark: Option<Theme>,
    pub light: Option<Theme>,
}

impl ThemeEntry {
    #[must_use]
    pub fn variants(&self) -> Vec<(&'static str, Theme)> {
        let mut variants = Vec::new();
        if let Some(theme) = self.dark {
            variants.push(("dark", theme));
        }
        if let Some(theme) = self.light {
            variants.push(("light", theme));
        }
        variants
    }
}

/// Built-in themes plus any parsed from theme files.
#[must_use]
pub fn builtin_entries() -> Vec<ThemeEntry> {
    let mut entries = vec![ThemeEntry {
        name: "editorial".into(),
        dark: Some(Theme::editorial_dark()),
        light: Some(Theme::editorial_light()),
    }];
    // The embedded theme collection.
    for (name, content) in crate::theme_embedded::EMBEDDED_THEMES {
        match parse_theme_file(name, content) {
            Ok(entry) => entries.push(entry),
            Err(error) => tracing::warn!(theme = name, error, "embedded theme skipped"),
        }
    }
    entries
}

#[derive(Deserialize)]
struct ThemeFile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    dark: Option<PaletteFile>,
    #[serde(default)]
    light: Option<PaletteFile>,
    // A flat palette (no [dark]/[light] tables) applies to both variants.
    #[serde(flatten)]
    flat: PaletteFile,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PaletteFile {
    bg: Option<String>,
    surface: Option<String>,
    overlay: Option<String>,
    selected: Option<String>,
    text: Option<String>,
    muted: Option<String>,
    primary: Option<String>,
    accent: Option<String>,
    user: Option<String>,
    danger: Option<String>,
    syntax: Option<SyntaxFile>,
    diff: Option<DiffFile>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SyntaxFile {
    keyword: Option<String>,
    string: Option<String>,
    comment: Option<String>,
    constant: Option<String>,
    function: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct DiffFile {
    added: Option<String>,
    added_bg: Option<String>,
    removed: Option<String>,
    removed_bg: Option<String>,
}

impl PaletteFile {
    fn is_empty(&self) -> bool {
        self.bg.is_none()
            && self.surface.is_none()
            && self.overlay.is_none()
            && self.selected.is_none()
            && self.text.is_none()
            && self.muted.is_none()
            && self.primary.is_none()
            && self.accent.is_none()
            && self.user.is_none()
            && self.danger.is_none()
            && self.syntax.is_none()
            && self.diff.is_none()
    }

    fn apply(&self, base: Theme) -> Result<Theme, String> {
        let mut theme = base;
        apply_color(self.bg.as_ref(), &mut theme.bg)?;
        apply_color(self.surface.as_ref(), &mut theme.surface)?;
        // A custom surface is also the natural overlay fallback. Themes can
        // split them explicitly when they want a different modal color.
        theme.overlay = theme.surface;
        apply_color(self.overlay.as_ref(), &mut theme.overlay)?;
        apply_color(self.selected.as_ref(), &mut theme.selected)?;
        apply_color(self.text.as_ref(), &mut theme.text)?;
        apply_color(self.muted.as_ref(), &mut theme.muted)?;
        apply_color(self.primary.as_ref(), &mut theme.primary)?;
        apply_color(self.accent.as_ref(), &mut theme.accent)?;
        apply_color(self.user.as_ref(), &mut theme.user)?;
        apply_color(self.danger.as_ref(), &mut theme.danger)?;
        if let Some(syntax) = &self.syntax {
            apply_color(syntax.keyword.as_ref(), &mut theme.syntax.keyword)?;
            apply_color(syntax.string.as_ref(), &mut theme.syntax.string)?;
            apply_color(syntax.comment.as_ref(), &mut theme.syntax.comment)?;
            apply_color(syntax.constant.as_ref(), &mut theme.syntax.constant)?;
            apply_color(syntax.function.as_ref(), &mut theme.syntax.function)?;
        }
        if let Some(diff) = &self.diff {
            apply_color(diff.added.as_ref(), &mut theme.diff.added)?;
            apply_color(diff.added_bg.as_ref(), &mut theme.diff.added_bg)?;
            apply_color(diff.removed.as_ref(), &mut theme.diff.removed)?;
            apply_color(diff.removed_bg.as_ref(), &mut theme.diff.removed_bg)?;
        }
        Ok(theme)
    }
}

fn apply_color(value: Option<&String>, target: &mut Color) -> Result<(), String> {
    if let Some(value) = value {
        *target = parse_hex(value)?;
    }
    Ok(())
}

/// `#rrggbb` (leading `#` optional).
pub fn parse_hex(value: &str) -> Result<Color, String> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(format!("invalid color {value:?}; expected #rrggbb"));
    }
    let component = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&hex[range], 16).map_err(|error| error.to_string())
    };
    Ok(Color::rgb(
        component(0..2)?,
        component(2..4)?,
        component(4..6)?,
    ))
}

/// Parse one theme file. Unset keys fall back to the editorial palette of the
/// matching variant, so partial themes stay coherent.
pub fn parse_theme_file(fallback_name: &str, content: &str) -> Result<ThemeEntry, String> {
    let file: ThemeFile = toml::from_str(content).map_err(|error| error.to_string())?;
    let name = file
        .name
        .clone()
        .unwrap_or_else(|| fallback_name.to_owned());
    let mut dark = None;
    let mut light = None;
    if let Some(palette) = &file.dark {
        dark = Some(palette.apply(Theme::editorial_dark())?);
    }
    if let Some(palette) = &file.light {
        light = Some(palette.apply(Theme::editorial_light())?);
    }
    if dark.is_none() && light.is_none() {
        if file.flat.is_empty() {
            return Err("theme file defines no colors".into());
        }
        dark = Some(file.flat.apply(Theme::editorial_dark())?);
        light = Some(file.flat.apply(Theme::editorial_light())?);
    }
    Ok(ThemeEntry { name, dark, light })
}

#[cfg(test)]
mod tests {
    use super::{Theme, parse_hex, parse_theme_file};
    use haarness_opentui::Color;

    #[test]
    fn hex_parsing_accepts_hash_prefixed_lowercase_and_upper() {
        assert_eq!(parse_hex("#102030").unwrap(), Color::rgb(16, 32, 48));
        assert_eq!(parse_hex("FFffFF").unwrap(), Color::rgb(255, 255, 255));
        assert!(parse_hex("#12345").is_err());
        assert!(parse_hex("nothex").is_err());
    }

    #[test]
    fn variant_tables_fall_back_to_editorial_for_unset_keys() {
        let entry = parse_theme_file(
            "custom",
            "name = \"Custom\"\n[dark]\nbg = \"#102030\"\n[dark.syntax]\nkeyword = \"#ff0000\"\n",
        )
        .unwrap();
        assert_eq!(entry.name, "Custom");
        let dark = entry.dark.unwrap();
        assert_eq!(dark.bg, Color::rgb(16, 32, 48));
        assert_eq!(dark.syntax.keyword, Color::rgb(255, 0, 0));
        // Unset keys keep the editorial values.
        assert_eq!(dark.text, Theme::editorial_dark().text);
        assert!(entry.light.is_none(), "no [light] table, no light variant");
    }

    #[test]
    fn flat_palettes_produce_both_variants() {
        let entry = parse_theme_file("flat", "primary = \"#0000ff\"\n").unwrap();
        assert_eq!(entry.dark.unwrap().primary, Color::rgb(0, 0, 255));
        assert_eq!(entry.light.unwrap().primary, Color::rgb(0, 0, 255));
        assert_eq!(
            entry.dark.unwrap().bg,
            Theme::editorial_dark().bg,
            "flat themes inherit each variant's base"
        );
    }

    #[test]
    fn custom_surfaces_supply_a_solid_overlay_fallback() {
        let inherited = parse_theme_file("custom", "surface = \"#102030\"\n").unwrap();
        let inherited = inherited.light.unwrap();
        assert_eq!(inherited.surface, Color::rgb(16, 32, 48));
        assert_eq!(inherited.overlay, inherited.surface);

        let explicit = parse_theme_file("custom", "surface = \"#102030\"\noverlay = \"#405060\"\n")
            .unwrap()
            .dark
            .unwrap();
        assert_eq!(explicit.surface, Color::rgb(16, 32, 48));
        assert_eq!(explicit.overlay, Color::rgb(64, 80, 96));
    }

    #[test]
    fn bad_color_values_are_rejected_with_context() {
        let error = parse_theme_file("broken", "[dark]\nbg = \"purple\"\n").unwrap_err();
        assert!(error.contains("purple"), "{error}");
        assert!(parse_theme_file("empty", "name = \"x\"\n").is_err());
    }
}

#[cfg(test)]
mod embedded_tests {
    #[test]
    fn every_embedded_theme_parses_with_both_variants_resolved() {
        let entries = super::builtin_entries();
        assert!(
            entries.len() >= 30,
            "expected the embedded collection, got {}",
            entries.len()
        );
        for name in ["tokyonight", "dracula", "gruvbox", "nord", "catppuccin"] {
            let entry = entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("missing embedded theme {name}"));
            assert!(entry.dark.is_some(), "{name} has a dark variant");
        }
        // Sanity: a converted color differs from the editorial base.
        let tokyo = entries
            .iter()
            .find(|entry| entry.name == "tokyonight")
            .unwrap();
        assert_ne!(
            tokyo.dark.as_ref().unwrap().bg,
            super::Theme::editorial_dark().bg
        );
    }
}
