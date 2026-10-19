//! Export the HTML study's colors from the pinned KIT dependency.
//! Run: nix develop -c cargo run --quiet --example export-design-palette
use starkit::native_surface::style::{hex, Palette, Style};
use starkit::theme::{Theme, ThemeFile, BUILTINS};

fn main() {
    let mut themes = serde_json::Map::new();
    for (name, id) in [
        ("kanagawa", "kanagawa"),
        ("classic", "winamp-classic"),
        ("mocha", "catppuccin-mocha"),
        ("latte", "catppuccin-latte"),
    ] {
        let builtin = BUILTINS.iter().find(|b| b.id == id).unwrap();
        let file: ThemeFile = toml::from_str(builtin.toml).unwrap();
        let theme = Theme::resolve(&file);
        let mut palette = if name == "classic" {
            Palette::default()
        } else {
            Palette::from_theme(&theme)
        };
        // Match AMP and FOLD's graphical Kanagawa adapter: slate frames,
        // with the original theme background retained for darker inset wells.
        if name == "kanagawa" {
            palette.bg = [theme.header_bg.r, theme.header_bg.g, theme.header_bg.b];
            let inset = theme.bg.mix(starkit::theme::BLACK, 0.12);
            palette.inset_bg = Some([inset.r, inset.g, inset.b]);
        }
        let s = Style::new(palette);
        themes.insert(
            name.into(),
            serde_json::json!({
                "bg": hex(palette.bg), "fg": hex(palette.fg),
                "dim": hex(palette.muted), "accent": hex(palette.accent),
                "selected": hex(palette.selected), "border": hex(palette.border),
                "error": hex(palette.error), "panel": s.colors.panel,
                "title": s.colors.title, "well": s.colors.inset,
                "hi": s.colors.highlight, "shadow": s.colors.shadow,
                "raised": s.colors.raised, "wp": s.workspace.panel,
                "wh": s.workspace.header, "ww": s.workspace.well,
                "line": s.workspace.line, "control": s.workspace.control,
                "hover": s.workspace.hover,
            }),
        );
    }
    println!("// Generated from pinned STAR/KIT by examples/export-design-palette.rs.");
    println!(
        "const KIT_PALETTES = {};",
        serde_json::to_string_pretty(&themes).unwrap()
    );
}
