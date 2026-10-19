//! Color schemes. `THEMES[0]` uses the terminal's own palette; the other 99
//! are popular editor/terminal schemes in 24-bit color, sorted by name.
use ratatui::style::Color;

#[derive(Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    /// Screen background (`Color::Reset` = leave the terminal's background).
    pub bg: Color,
    pub fg: Color,
    /// Secondary text: axis labels, sensor names, units, hints.
    pub dim: Color,
    pub border: Color,
    /// Titles, column headers, CPU tag.
    pub accent: Color,
    /// Background of the selected row.
    pub sel_bg: Color,
    pub ok: Color,
    pub warn: Color,
    pub crit: Color,
    /// Min / max columns.
    pub cool: Color,
    pub hot: Color,
    /// Text drawn on top of `accent`/level-colored badges.
    pub on_badge: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

#[allow(clippy::too_many_arguments)]
const fn t(
    name: &'static str,
    bg: u32,
    fg: u32,
    dim: u32,
    border: u32,
    accent: u32,
    sel_bg: u32,
    ok: u32,
    warn: u32,
    crit: u32,
    cool: u32,
    hot: u32,
) -> Theme {
    Theme {
        name,
        bg: rgb(bg),
        fg: rgb(fg),
        dim: rgb(dim),
        border: rgb(border),
        accent: rgb(accent),
        sel_bg: rgb(sel_bg),
        ok: rgb(ok),
        warn: rgb(warn),
        crit: rgb(crit),
        cool: rgb(cool),
        hot: rgb(hot),
        on_badge: rgb(bg),
    }
}

pub const THEMES: [Theme; 100] = [
    Theme {
        name: "Terminal",
        bg: Color::Reset,
        fg: Color::Reset,
        // A fixed grey from the 256-color range, not ANSI "bright black":
        // some palettes (Solarized) make that the background color.
        dim: Color::Indexed(244),
        border: Color::Reset,
        accent: Color::Cyan,
        sel_bg: Color::Rgb(40, 44, 52),
        ok: Color::Green,
        warn: Color::Yellow,
        crit: Color::Red,
        cool: Color::Blue,
        hot: Color::Magenta,
        on_badge: Color::Black,
    },
    // name                      bg        fg        dim       border    accent    sel_bg    ok        warn      crit      cool      hot
    t("Alucard",                   0xfffbeb, 0x1f1f1f, 0x6c664b, 0xcfcfde, 0x644ac9, 0xede8d0, 0x14710a, 0x846e15, 0xcb3a2a, 0x036a96, 0xa3144d),
    t("Andromeda",                 0x23262e, 0xd5ced9, 0xa0a1a7, 0x3d4148, 0x00e8c6, 0x2f3341, 0x96e072, 0xffe66d, 0xee5d43, 0x7cb7ff, 0xc74ded),
    t("Aura Dark",                 0x15141b, 0xedecee, 0x6d6d6d, 0x29263c, 0xa277ff, 0x1f1d2e, 0x61ffca, 0xffca85, 0xff6767, 0x82e2ff, 0xf694ff),
    t("Ayu Dark",                  0x0b0e14, 0xbfbdb6, 0x565b66, 0x3a3f4b, 0xe6b450, 0x273747, 0xaad94c, 0xffb454, 0xf07178, 0x59c2ff, 0xd2a6ff),
    t("Ayu Light",                 0xf8f9fa, 0x5c6166, 0x8a9199, 0xd8d8d7, 0xfa8d3e, 0xe7eaed, 0x86b300, 0xf2ae49, 0xf07171, 0x399ee6, 0xa37acc),
    t("Ayu Mirage",                0x1f2430, 0xcccac2, 0x707a8c, 0x3a4252, 0x73d0ff, 0x33415e, 0xd5ff80, 0xffd173, 0xf28779, 0x95e6cb, 0xdfbfff),
    t("Base16 Eighties",           0x2d2d2d, 0xd3d0c8, 0x747369, 0x515151, 0x6699cc, 0x393939, 0x99cc99, 0xffcc66, 0xf2777a, 0x66cccc, 0xcc99cc),
    t("Bluloco Dark",              0x282c34, 0xabb2bf, 0x636d83, 0x41444d, 0x3691ff, 0x3e4451, 0x3fc56b, 0xf9c859, 0xff6480, 0x10b1fe, 0xff78f8),
    t("Bluloco Light",             0xf9f9f9, 0x383a42, 0xa0a1a7, 0xd5d5d5, 0x275fe4, 0xe8e8e8, 0x23974a, 0xc5a332, 0xd52753, 0x27618d, 0x823ff1),
    t("Catppuccin Frappé",         0x303446, 0xc6d0f5, 0x737994, 0x51576d, 0xca9ee6, 0x414559, 0xa6d189, 0xe5c890, 0xe78284, 0x8caaee, 0xef9f76),
    t("Catppuccin Latte",          0xeff1f5, 0x4c4f69, 0x9ca0b0, 0xacb0be, 0x8839ef, 0xccd0da, 0x40a02b, 0xdf8e1d, 0xd20f39, 0x1e66f5, 0xfe640b),
    t("Catppuccin Macchiato",      0x24273a, 0xcad3f5, 0x6e738d, 0x494d64, 0xc6a0f6, 0x363a4f, 0xa6da95, 0xeed49f, 0xed8796, 0x8aadf4, 0xf5a97f),
    t("Catppuccin Mocha",          0x1e1e2e, 0xcdd6f4, 0x6c7086, 0x45475a, 0xcba6f7, 0x313244, 0xa6e3a1, 0xf9e2af, 0xf38ba8, 0x89b4fa, 0xfab387),
    t("Cobalt2",                   0x193549, 0xffffff, 0x7f9fbf, 0x1f4662, 0xff9d00, 0x0d3a58, 0x3ad900, 0xffc600, 0xff628c, 0x9effff, 0xfb94ff),
    t("Darcula",                   0x2b2b2b, 0xa9b7c6, 0x808080, 0x3c3f41, 0xcc7832, 0x214283, 0x6a8759, 0xffc66d, 0xbc3f3c, 0x6897bb, 0x9876aa),
    t("Dawnfox",                   0xfaf4ed, 0x575279, 0x9893a5, 0xcecacd, 0x286983, 0xd0d8d8, 0x618774, 0xea9d34, 0xb4637a, 0x56949f, 0x907aa9),
    t("Dayfox",                    0xf6f2ee, 0x3d2b5a, 0x837a72, 0xd3c7bb, 0x2848a9, 0xe7d2be, 0x396847, 0xac5402, 0xa5222f, 0x287980, 0x6e33ce),
    t("Doom One",                  0x282c34, 0xbbc2cf, 0x5b6268, 0x3f444a, 0x51afef, 0x2c323c, 0x98be65, 0xecbe7b, 0xff6c6b, 0x46d9ff, 0xc678dd),
    t("Dracula",                   0x282a36, 0xf8f8f2, 0x6272a4, 0x6272a4, 0xbd93f9, 0x44475a, 0x50fa7b, 0xf1fa8c, 0xff5555, 0x8be9fd, 0xff79c6),
    t("Duskfox",                   0x232136, 0xe0def4, 0x6e6a86, 0x44415a, 0x569fba, 0x433c59, 0xa3be8c, 0xf6c177, 0xeb6f92, 0x9ccfd8, 0xc4a7e7),
    t("Edge Dark",                 0x2c2e34, 0xc5cdd9, 0x7f8490, 0x414550, 0x6cb6eb, 0x3b3e48, 0xa0c980, 0xdeb974, 0xec7279, 0x5dbbc1, 0xd38aea),
    t("Edge Light",                0xfafafa, 0x4b505b, 0x8790a0, 0xdde2e7, 0x5079be, 0xeef1f4, 0x608e32, 0xbe7e05, 0xd05858, 0x3a8b84, 0xb05ccc),
    t("Embark",                    0x1e1c31, 0xcbe3e7, 0x8a889d, 0x3e3859, 0x91ddff, 0x2d2b40, 0xa1efd3, 0xffe6b3, 0xf48fb1, 0x87dfeb, 0xd4bfff),
    t("Everblush",                 0x141b1e, 0xdadada, 0x5d6467, 0x2d3437, 0x67b0e8, 0x232a2d, 0x8ccf7e, 0xe5c76b, 0xe57474, 0x6cbfbf, 0xc47fd5),
    t("Everforest",                0x2d353b, 0xd3c6aa, 0x859289, 0x56635f, 0xa7c080, 0x475258, 0xa7c080, 0xdbbc7f, 0xe67e80, 0x7fbbb3, 0xd699b6),
    t("Everforest Light",          0xfdf6e3, 0x5c6a72, 0x939f91, 0xe0dcc7, 0x8da101, 0xe6e2cc, 0x8da101, 0xdfa000, 0xf85552, 0x3a94c5, 0xdf69ba),
    t("Flexoki Dark",              0x100f0f, 0xcecdc3, 0x878580, 0x343331, 0x4385be, 0x282726, 0x879a39, 0xd0a215, 0xd14d41, 0x3aa99f, 0xce5d97),
    t("Flexoki Light",             0xfffcf0, 0x100f0f, 0x6f6e69, 0xcecdc3, 0x205ea6, 0xe6e4d9, 0x66800b, 0xad8301, 0xaf3029, 0x24837b, 0xa02f6f),
    t("GitHub Dark",               0x0d1117, 0xc9d1d9, 0x8b949e, 0x30363d, 0x58a6ff, 0x21262d, 0x3fb950, 0xd29922, 0xf85149, 0x79c0ff, 0xbc8cff),
    t("GitHub Dark Dimmed",        0x22272e, 0xadbac7, 0x768390, 0x444c56, 0x539bf5, 0x2d333b, 0x57ab5a, 0xc69026, 0xe5534b, 0x6cb6ff, 0xb083f0),
    t("GitHub Dark High Contrast", 0x0a0c10, 0xf0f3f6, 0x9ea7b3, 0x7a828e, 0x71b7ff, 0x272b33, 0x26cd4d, 0xf0b72f, 0xff6a69, 0x56d4dd, 0xcb9eff),
    t("GitHub Light",              0xffffff, 0x24292f, 0x6e7781, 0xd0d7de, 0x0969da, 0xeaeef2, 0x1a7f37, 0x9a6700, 0xcf222e, 0x1b7c83, 0x8250df),
    t("Gruvbox Dark",              0x282828, 0xebdbb2, 0x928374, 0x665c54, 0xfabd2f, 0x3c3836, 0xb8bb26, 0xfe8019, 0xfb4934, 0x83a598, 0xd3869b),
    t("Gruvbox Dark Hard",         0x1d2021, 0xebdbb2, 0x928374, 0x504945, 0xfabd2f, 0x3c3836, 0xb8bb26, 0xfe8019, 0xfb4934, 0x83a598, 0xd3869b),
    t("Gruvbox Light",             0xfbf1c7, 0x3c3836, 0x928374, 0xbdae93, 0xaf3a03, 0xebdbb2, 0x79740e, 0xb57614, 0x9d0006, 0x076678, 0x8f3f71),
    t("Gruvbox Material",          0x282828, 0xd4be98, 0x928374, 0x504945, 0xe78a4e, 0x3c3836, 0xa9b665, 0xd8a657, 0xea6962, 0x7daea3, 0xd3869b),
    t("Gruvbox Material Light",    0xfbf1c7, 0x654735, 0x928374, 0xd5c4a1, 0xc35e0a, 0xf2e5bc, 0x6c782e, 0xb47109, 0xc14a4a, 0x45707a, 0x945e80),
    t("Horizon",                   0x1c1e26, 0xd5d8da, 0x6c6f93, 0x2e303e, 0xee64ac, 0x2e303e, 0x29d398, 0xfab795, 0xe95678, 0x26bbd9, 0xf09483),
    t("Iceberg",                   0x161821, 0xc6c8d1, 0x6b7089, 0x444b71, 0x84a0c6, 0x272c42, 0xb4be82, 0xe2a478, 0xe27878, 0x89b8c2, 0xa093c7),
    t("Jellybeans",                0x151515, 0xe8e8d3, 0x888888, 0x404040, 0x8197bf, 0x2a2a2a, 0x99ad6a, 0xfad07a, 0xcf6a4c, 0x8fbfdc, 0xc6b6ee),
    t("Kanagawa",                  0x1f1f28, 0xdcd7ba, 0x727169, 0x54546d, 0x7e9cd8, 0x2d4f67, 0x98bb6c, 0xe6c384, 0xff5d62, 0x7fb4ca, 0x957fb8),
    t("Kanagawa Dragon",           0x181616, 0xc5c9c5, 0x737c73, 0x625e5a, 0x8ba4b0, 0x223249, 0x87a987, 0xc4b28a, 0xc4746e, 0x8ea4a2, 0xa292a3),
    t("Kanagawa Lotus",            0xf2ecbc, 0x545464, 0x8a8980, 0xa09cac, 0x4d699b, 0xc9cbd1, 0x6f894e, 0x77713f, 0xc84053, 0x597b75, 0x624c83),
    t("Material Darker",           0x212121, 0xeeffff, 0x545454, 0x424242, 0xff9800, 0x2c2c2c, 0xc3e88d, 0xffcb6b, 0xf07178, 0x89ddff, 0xc792ea),
    t("Material Lighter",          0xfafafa, 0x546e7a, 0x90a4ae, 0xd3e1e8, 0x00bcd4, 0xe7e7e8, 0x91b859, 0xf6a434, 0xe53935, 0x39adb5, 0x7c4dff),
    t("Material Ocean",            0x0f111a, 0x8f93a2, 0x464b5d, 0x1f2233, 0x84ffff, 0x1a1c25, 0xc3e88d, 0xffcb6b, 0xf07178, 0x89ddff, 0xc792ea),
    t("Material Palenight",        0x292d3e, 0xa6accd, 0x676e95, 0x4e5579, 0xc792ea, 0x444267, 0xc3e88d, 0xffcb6b, 0xf07178, 0x89ddff, 0xf78c6c),
    t("Modus Operandi",            0xffffff, 0x000000, 0x595959, 0x9f9f9f, 0x0031a9, 0xc0deff, 0x006800, 0x6f5500, 0xa60000, 0x005e8b, 0x721045),
    t("Modus Vivendi",             0x000000, 0xffffff, 0x989898, 0x646464, 0x2fafff, 0x2f3849, 0x44bc44, 0xd0bc00, 0xff5f59, 0x00d3d0, 0xf78fe7),
    t("Molokai",                   0x1b1d1e, 0xf8f8f2, 0x7e8e91, 0x465457, 0xfd971f, 0x403d3d, 0xa6e22e, 0xe6db74, 0xf92672, 0x66d9ef, 0xae81ff),
    t("Monokai",                   0x272822, 0xf8f8f2, 0x75715e, 0x75715e, 0x66d9ef, 0x49483e, 0xa6e22e, 0xe6db74, 0xf92672, 0x66d9ef, 0xae81ff),
    t("Monokai Pro",               0x2d2a2e, 0xfcfcfa, 0x727072, 0x5b595c, 0xab9df2, 0x403e41, 0xa9dc76, 0xffd866, 0xff6188, 0x78dce8, 0xfc9867),
    t("Monokai Pro Machine",       0x273136, 0xf2fffc, 0x6b7678, 0x545f62, 0x7cd5f1, 0x3a4449, 0xa2e57b, 0xffed72, 0xff6d7e, 0x7cd5f1, 0xbaa0f8),
    t("Monokai Pro Octagon",       0x282a3a, 0xeaf2f1, 0x696d77, 0x535763, 0xc39ac9, 0x3a3d4b, 0xbad761, 0xffd76d, 0xff657a, 0x9cd1bb, 0xff9b5e),
    t("Monokai Pro Ristretto",     0x2c2525, 0xfff1f3, 0x72696a, 0x5b5353, 0xa8a9eb, 0x403838, 0xadda78, 0xf9cc6c, 0xfd6883, 0x85dacc, 0xf38d70),
    t("Monokai Pro Spectrum",      0x222222, 0xf7f1ff, 0x69676c, 0x525053, 0x948ae3, 0x363537, 0x7bd88f, 0xfce566, 0xfc618d, 0x5ad4e6, 0xfd9353),
    t("Moonfly",                   0x080808, 0xc6c6c6, 0x808080, 0x323437, 0x80a0ff, 0x303030, 0x8cc85f, 0xe3c78a, 0xff5d5d, 0x79dac8, 0xae81ff),
    t("Night Owl",                 0x011627, 0xd6deeb, 0x637777, 0x2c4a63, 0x82aaff, 0x1d3b53, 0xaddb67, 0xecc48d, 0xef5350, 0x7fdbca, 0xc792ea),
    t("Night Owl Light",           0xfbfbfb, 0x403f53, 0x989fb1, 0xd9d9d9, 0x4876d6, 0xe0e7ea, 0x08916a, 0xdaaa01, 0xde3d3b, 0x2aa298, 0x994cc3),
    t("Nightfly",                  0x011627, 0xbdc1c6, 0x7c8f8f, 0x1d3b53, 0x82aaff, 0x0e293f, 0xa1cd5e, 0xe3d18a, 0xfc514e, 0x7fdbca, 0xc792ea),
    t("Nightfox",                  0x192330, 0xcdcecf, 0x738091, 0x39506d, 0x719cd6, 0x2b3b51, 0x81b29a, 0xdbc074, 0xc94f6d, 0x63cdcf, 0x9d79d6),
    t("Nord",                      0x2e3440, 0xd8dee9, 0x616e88, 0x4c566a, 0x88c0d0, 0x434c5e, 0xa3be8c, 0xebcb8b, 0xbf616a, 0x81a1c1, 0xb48ead),
    t("Nordfox",                   0x2e3440, 0xcdcecf, 0x60728a, 0x444c5e, 0x81a1c1, 0x3e4a5b, 0xa3be8c, 0xebcb8b, 0xbf616a, 0x88c0d0, 0xb48ead),
    t("Oceanic Next",              0x1b2b34, 0xc0c5ce, 0x65737e, 0x343d46, 0x6699cc, 0x4f5b66, 0x99c794, 0xfac863, 0xec5f67, 0x5fb3b3, 0xc594c5),
    t("One Dark",                  0x282c34, 0xabb2bf, 0x5c6370, 0x5c6370, 0x61afef, 0x3e4451, 0x98c379, 0xe5c07b, 0xe06c75, 0x56b6c2, 0xc678dd),
    t("One Light",                 0xfafafa, 0x383a42, 0xa0a1a7, 0xd4d4d4, 0x4078f2, 0xe5e5e6, 0x50a14f, 0xc18401, 0xe45649, 0x0184bc, 0xa626a4),
    t("Panda",                     0x292a2b, 0xe6e6e6, 0x676b79, 0x3e4044, 0xb084eb, 0x31353a, 0x19f9d8, 0xffcc95, 0xff2c6d, 0x45a9f9, 0xff75b5),
    t("PaperColor Dark",           0x1c1c1c, 0xd0d0d0, 0x808080, 0x3a3a3a, 0x5fafd7, 0x303030, 0x5faf00, 0xd7af5f, 0xaf005f, 0x00afaf, 0xaf87d7),
    t("PaperColor Light",          0xeeeeee, 0x444444, 0x878787, 0xbcbcbc, 0x005f87, 0xd0d0d0, 0x718c00, 0xd75f00, 0xaf0000, 0x0087af, 0x8700af),
    t("Poimandres",                0x1b1e28, 0xa6accd, 0x767c9d, 0x303340, 0x89ddff, 0x303340, 0x5de4c7, 0xfffac2, 0xd0679d, 0xadd7ff, 0xfcc5e9),
    t("Rosé Pine",                 0x191724, 0xe0def4, 0x6e6a86, 0x403d52, 0xc4a7e7, 0x26233a, 0x9ccfd8, 0xf6c177, 0xeb6f92, 0x31748f, 0xebbcba),
    t("Rosé Pine Dawn",            0xfaf4ed, 0x575279, 0x9893a5, 0xcecacd, 0x907aa9, 0xdfdad9, 0x56949f, 0xea9d34, 0xb4637a, 0x286983, 0xd7827e),
    t("Rosé Pine Moon",            0x232136, 0xe0def4, 0x6e6a86, 0x44415a, 0xc4a7e7, 0x393552, 0x9ccfd8, 0xf6c177, 0xeb6f92, 0x3e8fb0, 0xea9a97),
    t("Selenized Black",           0x181818, 0xb9b9b9, 0x777777, 0x3b3b3b, 0x368aeb, 0x252525, 0x70b433, 0xdbb32d, 0xed4a46, 0x3fc5b7, 0xeb6eb7),
    t("Selenized Dark",            0x103c48, 0xadbcbc, 0x72898f, 0x2d5b69, 0x4695f7, 0x184956, 0x75b938, 0xdbb32d, 0xfa5750, 0x41c7b9, 0xf275be),
    t("Selenized Light",           0xfbf3db, 0x53676d, 0x909995, 0xd5cdb6, 0x0072d4, 0xece3cc, 0x489100, 0xad8900, 0xd2212d, 0x009c8f, 0xca4898),
    t("Shades of Purple",          0x2d2b55, 0xffffff, 0xa599e9, 0x5d37f0, 0xff9d00, 0x1e1e3f, 0xa5ff90, 0xfad000, 0xff628c, 0x9effff, 0xfb94ff),
    t("Snazzy",                    0x282a36, 0xeff0eb, 0x78787e, 0x43454f, 0x57c7ff, 0x34353e, 0x5af78e, 0xf3f99d, 0xff5c57, 0x9aedfe, 0xff6ac1),
    t("Solarized Dark",            0x002b36, 0x839496, 0x586e75, 0x586e75, 0x268bd2, 0x073642, 0x859900, 0xb58900, 0xdc322f, 0x2aa198, 0xd33682),
    t("Solarized Light",           0xfdf6e3, 0x657b83, 0x93a1a1, 0x93a1a1, 0x268bd2, 0xeee8d5, 0x859900, 0xb58900, 0xdc322f, 0x2aa198, 0xd33682),
    t("Sonokai",                   0x2c2e34, 0xe2e2e3, 0x7f8490, 0x414550, 0x76cce0, 0x3b3e48, 0x9ed072, 0xe7c664, 0xfc5d7c, 0x76cce0, 0xb39df3),
    t("Spacegray",                 0x2b303b, 0xc0c5ce, 0x65737e, 0x4f5b66, 0x8fa1b3, 0x343d46, 0xa3be8c, 0xebcb8b, 0xbf616a, 0x96b5b4, 0xb48ead),
    t("Synthwave '84",             0x262335, 0xf0eff1, 0x848bbd, 0x495495, 0xff7edb, 0x34294f, 0x72f1b8, 0xfede5d, 0xfe4450, 0x36f9f6, 0xf97e72),
    t("Tango Dark",                0x2e3436, 0xd3d7cf, 0x888a85, 0x555753, 0x729fcf, 0x3c4244, 0x8ae234, 0xfce94f, 0xef2929, 0x34e2e2, 0xad7fa8),
    t("Tango Light",               0xffffff, 0x2e3436, 0x888a85, 0xd3d7cf, 0x3465a4, 0xeeeeec, 0x4e9a06, 0xc4a000, 0xcc0000, 0x06989a, 0x75507b),
    t("Tokyo Night",               0x1a1b26, 0xc0caf5, 0x565f89, 0x3b4261, 0x7aa2f7, 0x283457, 0x9ece6a, 0xe0af68, 0xf7768e, 0x7dcfff, 0xbb9af7),
    t("Tokyo Night Day",           0xe1e2e7, 0x3760bf, 0x848cb5, 0xa8aecb, 0x2e7de9, 0xb7c1e3, 0x587539, 0x8c6c3e, 0xf52a65, 0x007197, 0x9854f1),
    t("Tokyo Night Moon",          0x222436, 0xc8d3f5, 0x636da6, 0x3b4261, 0x82aaff, 0x2d3f76, 0xc3e88d, 0xffc777, 0xff757f, 0x86e1fc, 0xc099ff),
    t("Tokyo Night Storm",         0x24283b, 0xc0caf5, 0x565f89, 0x3b4261, 0x7aa2f7, 0x2e3c64, 0x9ece6a, 0xe0af68, 0xf7768e, 0x7dcfff, 0xbb9af7),
    t("Tomorrow",                  0xffffff, 0x4d4d4c, 0x8e908c, 0xd6d6d6, 0x4271ae, 0xefefef, 0x718c00, 0xeab700, 0xc82829, 0x3e999f, 0x8959a8),
    t("Tomorrow Night",            0x1d1f21, 0xc5c8c6, 0x969896, 0x4d5057, 0x81a2be, 0x373b41, 0xb5bd68, 0xf0c674, 0xcc6666, 0x8abeb7, 0xb294bb),
    t("Tomorrow Night Blue",       0x002451, 0xffffff, 0x7285b7, 0x00346e, 0xbbdaff, 0x003f8e, 0xd1f1a9, 0xffeead, 0xff9da4, 0x99ffff, 0xebbbff),
    t("Tomorrow Night Bright",     0x000000, 0xeaeaea, 0x969896, 0x424242, 0x7aa6da, 0x2a2a2a, 0xb9ca4a, 0xe7c547, 0xd54e53, 0x70c0b1, 0xc397d8),
    t("Ubuntu",                    0x300a24, 0xeeeeec, 0x888a85, 0x5c3566, 0x729fcf, 0x4e2a4a, 0x8ae234, 0xfce94f, 0xef2929, 0x34e2e2, 0xad7fa8),
    t("VS Code Dark+",             0x1e1e1e, 0xd4d4d4, 0x808080, 0x3c3c3c, 0x569cd6, 0x264f78, 0x0dbc79, 0xe5e510, 0xf14c4c, 0x4ec9b0, 0xc586c0),
    t("VS Code Light+",            0xffffff, 0x000000, 0x808080, 0xd4d4d4, 0x0066b8, 0xadd6ff, 0x008000, 0x795e26, 0xcd3131, 0x267f99, 0xaf00db),
    t("Xcode Dark",                0x1f1f24, 0xffffff, 0x6c7986, 0x393b40, 0xa167e6, 0x515b70, 0x67b7a4, 0xd0bf69, 0xfc6a5d, 0x5dd8ff, 0xfc5fa3),
    t("Xcode Light",               0xffffff, 0x262626, 0x707f8c, 0xd1d1d1, 0x0b4f79, 0xb4d8fd, 0x326d74, 0x815f03, 0xd12f1b, 0x0f68a0, 0xad3da4),
    t("Zenburn",                   0x3f3f3f, 0xdcdccc, 0x709080, 0x6f6f6f, 0x8cd0d3, 0x4f4f4f, 0x9fc59f, 0xf0dfaf, 0xcc9393, 0x94bff3, 0xdc8cc3),
];

/// Find a theme by name, ignoring case, spaces, dashes and punctuation
/// ("tokyo-night", "TokyoNight" and "Tokyo Night" all match).
pub fn find(name: &str) -> Option<usize> {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .map(|c| if c == 'é' { 'e' } else { c })
            .collect::<String>()
    };
    let want = norm(name);
    THEMES.iter().position(|t| norm(t.name) == want)
}

// ---------------------------------------------------------------------------
// Legibility
// ---------------------------------------------------------------------------

/// WCAG relative luminance.
fn luminance(c: Color) -> Option<f32> {
    let Color::Rgb(r, g, b) = c else { return None };
    let ch = |v: u8| {
        let x = v as f32 / 255.0;
        if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    };
    Some(0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b))
}

fn contrast(a: Color, b: Color) -> Option<f32> {
    let (la, lb) = (luminance(a)?, luminance(b)?);
    Some((la.max(lb) + 0.05) / (la.min(lb) + 0.05))
}

impl Theme {
    /// Status and accent colors carry meaning here (warm / critical), so a
    /// scheme's color that is too faint on its own background (e.g. the pale
    /// yellows of some light themes) is darkened — or lightened on dark
    /// themes — just until it is readable, keeping its hue. Colors that are
    /// already readable are left exactly as the scheme defines them.
    pub fn legible(mut self) -> Theme {
        const MIN: f32 = 3.0;
        let bg = self.bg;
        let Some(bg_l) = luminance(bg) else { return self };
        let fix = |c: Color| -> Color {
            if contrast(c, bg).is_none_or(|r| r >= MIN) {
                return c;
            }
            let Some((h, s, mut l)) = to_hsl(c) else { return c };
            let step = if bg_l > 0.4 { -0.02 } else { 0.02 };
            let mut out = c;
            for _ in 0..50 {
                l = (l + step).clamp(0.0, 1.0);
                out = hsl(h, s, l);
                if contrast(out, bg).is_some_and(|r| r >= MIN) {
                    break;
                }
            }
            out
        };
        self.ok = fix(self.ok);
        self.warn = fix(self.warn);
        self.crit = fix(self.crit);
        self.accent = fix(self.accent);
        self.cool = fix(self.cool);
        self.hot = fix(self.hot);
        self
    }
}

// ---------------------------------------------------------------------------
// Category label colors
// ---------------------------------------------------------------------------

/// Each category's hue on the color wheel (degrees). Related categories sit
/// near each other (storage = warm oranges, connectivity = pinks/cyans) but
/// every one is distinct.
fn kind_hue(kind: &str) -> Option<f32> {
    Some(match kind {
        "CPU" => 212.0,   // blue
        "GPU" => 145.0,   // green
        "RAM" => 268.0,   // violet
        "NVME" => 28.0,   // orange
        "SSD" => 46.0,    // amber
        "HDD" => 10.0,    // coral
        "DISK" => 36.0,
        "USB" => 322.0,   // pink
        "NET" => 178.0,   // teal
        "WIFI" => 194.0,  // cyan
        "AUDIO" => 295.0, // magenta
        "MB" => 88.0,     // lime
        "CHIP" => 238.0,  // indigo
        "BAT" => 62.0,    // yellow
        _ => return None, // ACPI and unknown: neutral grey
    })
}

/// Text color for a category label, tuned to the theme: the saturation and
/// lightness are taken from the theme's own accent colors, so badges look
/// soft on muted schemes (Nord, Everforest), vivid on bold ones (Synthwave,
/// Dracula), and darker on light schemes so the label stays readable on the light background.
pub fn kind_color(kind: &str, th: &Theme) -> Color {
    let light_theme = matches!(th.bg, Color::Rgb(..)) && lightness(th.bg) > 0.5;
    let (mut sat, mut lum) = {
        let samples: Vec<(f32, f32)> =
            [th.accent, th.cool, th.hot, th.ok].iter().filter_map(|c| to_hsl(*c)).map(|(_, s, l)| (s, l)).collect();
        if samples.is_empty() {
            (0.65, 0.68) // the Terminal theme: no RGB colors to learn from
        } else {
            let n = samples.len() as f32;
            (samples.iter().map(|x| x.0).sum::<f32>() / n, samples.iter().map(|x| x.1).sum::<f32>() / n)
        }
    };
    if light_theme {
        sat = sat.clamp(0.45, 0.75);
        lum = lum.clamp(0.36, 0.46);
    } else {
        sat = sat.clamp(0.40, 0.80);
        lum = lum.clamp(0.62, 0.74);
    }
    match kind_hue(kind) {
        Some(h) => hsl(h, sat, lum),
        None => hsl(0.0, 0.0, if light_theme { 0.45 } else { 0.62 }),
    }
}

fn lightness(c: Color) -> f32 {
    to_hsl(c).map(|(_, _, l)| l).unwrap_or(0.0)
}

fn to_hsl(c: Color) -> Option<(f32, f32, f32)> {
    let Color::Rgb(r, g, b) = c else { return None };
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return Some((0.0, 0.0, l));
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    Some(((h + 360.0) % 360.0, s, l))
}

fn hsl(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(to(r), to(g), to(b))
}

// ---------------------------------------------------------------------------
// Remembering the choice
// ---------------------------------------------------------------------------

/// `~/.config/oryn` (or `$XDG_CONFIG_HOME/oryn`, `%APPDATA%\oryn`).
///
/// Oryn used to be called tempmon: if `oryn` doesn't exist yet but the old
/// `tempmon` folder does, its settings (theme, panels, layout, compact mode)
/// are copied over once, so nothing is lost. The old folder is left alone.
fn config_dir() -> Option<std::path::PathBuf> {
    // Under `sudo oryn` (needed for SMART and the RTX 50 hotspot), use the
    // invoking user's settings rather than root's, so the theme and layout
    // you chose are the ones you get.
    let base = sudo_user()
        .map(|(_, _, home)| home.join(".config"))
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(std::path::PathBuf::from))?;
    let dir = base.join("oryn");
    let old = base.join("tempmon");
    if !dir.exists() && old.is_dir()
        && std::fs::create_dir_all(&dir).is_ok() {
            for e in std::fs::read_dir(&old).into_iter().flatten().flatten() {
                if e.path().is_file() {
                    let _ = std::fs::copy(e.path(), dir.join(e.file_name()));
                    give_back(&dir.join(e.file_name()));
                }
            }
            give_back(&dir);
        }
    Some(dir)
}

/// The user who ran `sudo oryn`: (uid, gid, home). None when not under sudo.
#[cfg(unix)]
fn sudo_user() -> Option<(u32, u32, std::path::PathBuf)> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return None;
    }
    let uid: u32 = std::env::var("SUDO_UID").ok()?.parse().ok()?;
    if uid == 0 {
        return None;
    }
    // SAFETY: getpwuid returns a pointer to static storage or null; the
    // fields are copied out before anything else can call it.
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let home = std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().to_string();
        Some((uid, (*pw).pw_gid, std::path::PathBuf::from(home)))
    }
}

#[cfg(not(unix))]
fn sudo_user() -> Option<(u32, u32, std::path::PathBuf)> {
    None
}

/// Under sudo, hand a settings file/folder we created back to the user, so
/// a later run without sudo can still save over it.
pub(crate) fn give_back(path: &std::path::Path) {
    #[cfg(unix)]
    if let Some((uid, gid, _)) = sudo_user() {
        let _ = std::os::unix::fs::chown(path, Some(uid), Some(gid));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Write one settings file (creating the folder if needed).
pub fn write_config(name: &str, contents: &str) {
    // Tests change the theme, layout and compact mode; they must never
    // overwrite the settings of whoever runs them.
    if cfg!(test) {
        return;
    }
    let Some(dir) = config_dir() else { return };
    let fresh = !dir.exists();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if fresh {
        give_back(&dir);
        if let Some(parent) = dir.parent() {
            // ~/.config itself, if we just created it.
            if std::fs::metadata(parent).is_ok_and(|m| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    m.uid() == 0
                }
                #[cfg(not(unix))]
                {
                    let _ = m;
                    false
                }
            }) {
                give_back(parent);
            }
        }
    }
    let path = dir.join(name);
    if std::fs::write(&path, contents).is_ok() {
        give_back(&path);
    }
}

/// Read one settings file, if it exists.
pub fn read_config(name: &str) -> Option<String> {
    std::fs::read_to_string(config_dir()?.join(name)).ok()
}

pub fn load_saved() -> Option<usize> {
    find(read_config("theme")?.trim())
}

pub fn save(idx: usize) {
    write_config("theme", &format!("{}\n", THEMES[idx].name));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ninety_nine_named_schemes_plus_terminal() {
        assert_eq!(THEMES.len(), 100);
        let mut names: Vec<_> = THEMES.iter().map(|t| t.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 100);
    }

    #[test]
    fn hsl_round_trip() {
        for c in [0x7aa2f7u32, 0xff5555, 0x50fa7b, 0x282a36] {
            let col = rgb(c);
            let (h, s, l) = to_hsl(col).unwrap();
            let Color::Rgb(r1, g1, b1) = col else { unreachable!() };
            let Color::Rgb(r2, g2, b2) = hsl(h, s, l) else { unreachable!() };
            assert!((r1 as i16 - r2 as i16).abs() <= 1 && (g1 as i16 - g2 as i16).abs() <= 1 && (b1 as i16 - b2 as i16).abs() <= 1);
        }
    }

    #[test]
    fn badges_are_distinct_and_readable() {
        let kinds = ["CPU", "GPU", "RAM", "NVME", "SSD", "HDD", "USB", "NET", "WIFI", "MB", "CHIP"];
        for th in THEMES.iter() {
            let cols: Vec<Color> = kinds.iter().map(|k| kind_color(k, th)).collect();
            for i in 0..cols.len() {
                for j in i + 1..cols.len() {
                    assert_ne!(cols[i], cols[j], "{}: {} vs {}", th.name, kinds[i], kinds[j]);
                }
            }
            // Label color must contrast with the theme background it is drawn on.
            if let Color::Rgb(..) = th.on_badge {
                for c in &cols {
                    assert!((lightness(*c) - lightness(th.on_badge)).abs() > 0.3, "{} low contrast", th.name);
                }
            }
        }
    }

    #[test]
    fn status_colors_readable_on_every_scheme() {
        for t in THEMES.iter().skip(1) {
            let t = t.legible();
            for (what, c) in [("ok", t.ok), ("warn", t.warn), ("crit", t.crit), ("accent", t.accent)] {
                let r = contrast(c, t.bg).unwrap();
                assert!(r >= 2.9, "{} {what} contrast {r:.2}", t.name);
            }
        }
        // Readable colors are left untouched.
        let d = THEMES[find("dracula").unwrap()];
        assert_eq!(d.legible().warn, d.warn);
    }

    #[test]
    fn lookup_is_forgiving() {
        assert_eq!(find("tokyo-night"), find("Tokyo Night"));
        assert!(find("rose-pine").is_some());
        assert!(find("synthwave84").is_some());
        assert!(find("nope").is_none());
    }
}
