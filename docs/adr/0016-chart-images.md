# 0016 — Chart images

Status: Accepted (2026-10-27)

## Context
The `show` tool (ADR 0015) draws a card's bar chart as text (`Tea \`▇▇▇▇\` 3 kg`). That works
everywhere but reads poorly beyond a few rows, cannot show a trend, and misaligns in proportional
fonts. Our bots speak Vietnamese, English and Japanese. A VPS has no system fonts. No adapter can
send a file yet: `OutboundMessage.attachments` is unused on the way out, and `RenderedMessage` holds
text only.

A prototype (tiny-skia + ttf-parser, embedded subset fonts) rendered a mixed Vietnamese, Japanese
and English bar chart in about 11 ms. It produced a 120 KB PNG, and the whole binary was 1.9 MB with
its fonts. An SVG pipeline (resvg) gave the same picture with about 50 crates instead of about 20,
and it needs an SVG parser that we do not need.

## Decision

### A small renderer behind a port
- `okbot-core` gains `Chart`, `ChartKind`, `Image` and `trait ChartRenderer { fn render(&self,
  &Chart) -> Result<Image, ChartError> }`. The trait is synchronous and object-safe; the runtime
  calls it in `spawn_blocking`.
- A new crate, `okbot-chart`, depends on `okbot-core` only and implements the trait. `okbot` wires
  it in. The runtime depends on the port, never on the crate.
- **Dependencies:** `tiny-skia` (paths and PNG), `ttf-parser` (glyph outlines),
  `unicode-normalization` and `flate2`. The last three are already in the tree. There is no SVG
  parser, no text-shaping engine and no system-font lookup.
- **Input is data, never markup.** The agent sends labels and numbers, and okbot draws them. The
  agent cannot pass SVG, HTML, colours, URLs or sizes.

### Three kinds, chosen for chat
| Kind | Use | Limits |
| --- | --- | --- |
| `bar` (default) | comparing and ranking; long or CJK labels | 1 series, up to 15 bars |
| `column` | short categories in order (months, days) | 1–3 series, up to 12 categories |
| `line` | trends over time | 1–3 series, up to 60 points |

There is no pie, stacked, area or dual-axis chart. A share is a `bar` chart with percentages.

The `show` arguments stay backward compatible:
- `chart: {kind?, unit?, points}` keeps working. The card's title is the chart's title, so `show`
  draws none in the image.
- Several series use `chart: {kind, unit?, labels, series: [{name, values}]}`.
- `highlight` names one label to draw in the accent colour; the others are muted.
- `values` may hold `null` for a gap.

### Text in three languages without a shaping engine
- **No shaping is needed for these three languages.** Vietnamese has a precomposed code point
  for every letter, and Japanese needs no contextual shaping when written horizontally.
- **Every string is normalized to NFC** first, so decomposed input (`o` + `̛` + `̉`) becomes `ở`.
- **Fonts and weights.** Fonts are Noto Sans and Noto Sans JP (OFL-1.1). They are subset at build
  time by `crates/okbot-chart/fonts/build.sh` (fonttools, pinned) and stored deflated:

  | Font | Characters | Weights | Size (deflated) |
  | --- | --- | --- | --- |
  | Noto Sans | Latin, Latin Extended-A, Latin Extended Additional (Vietnamese), currency signs (₫ ¥ € $), arrows, ▲▼ | Regular, Bold | about 67 KB |
  | Noto Sans JP | kana, CJK punctuation, full-width forms, JIS level 1 kanji (2,965) | Regular only | about 600 KB |

  Bold Japanese text is drawn by also stroking the outline. Fonts are inflated once, on first use
  (about 18 ms).
- **Fallback chain.** Glyphs are looked up in this order:
  1. Noto Sans;
  2. Noto Sans JP;
  3. any fonts from `[charts] fonts` (for example, a Chinese or Korean font).
- **Missing glyphs.**
  - Emoji and symbols that no font covers are removed from labels.
  - Any other missing character means that chart is drawn as text, as it is today; it never shows
    tofu (□).
- **Tight labels.** A label that does not fit is cut at its measured width with `…`. The full label
  stays in the text version.
- **Line height** is 1.45 em, so stacked Vietnamese marks (`Ẫ`, `ỗ`) never touch the line above.

### Numbers by locale
The locale comes from the first of these that is set:
1. the chart's `locale`;
2. the event's reported locale (Discord, Telegram);
3. the script of the title and labels (kana or kanji means `ja`, Vietnamese letters mean `vi`);
4. `en`.

| | Full | Decimal | Compact |
| --- | --- | --- | --- |
| `vi` | 1.234.500 | 1,5 | 1,2 nghìn · 3,4 tr · 1,2 tỷ |
| `en` | 1,234,500 | 1.5 | 1.2K · 3.4M · 1.2B |
| `ja` | 1,234,500 | 1.5 | 1.2万 · 3,456万 · 1.2億 (myriads) |

- **Where numbers appear.** Bars carry their full value. Axes use compact values on "nice" ticks
  (1–2–5 steps, 4–5 ticks). Bars and columns always start at zero; a zero line is drawn when values
  are negative.
- **The unit** is written once, in the subtitle. The `₫` sign follows the number (`120.000 ₫`); the
  `$` and `¥` signs come before it.

### Made for phones as well as desktops
- **Width.** The logical width is 480 px, rendered at 2.5× to give 1200 px. Telegram keeps up to
  1280 px, and Discord scales the image down. On a 360 dp phone, 15 px labels show at about 11 dp.
- **Height** follows the data and stays at most 1.4 × the width, so previews do not crop it.
- **Background.** The chart is a white card with a 1 px border and 12 px corners, so it reads the
  same in light and dark chat themes.
- **Colours.**
  - A single series uses the card's tone colour.
  - Several series use three colour-blind-safe colours (blue, orange, teal). Lines also differ by
    dash and marker, not by colour alone.
  - Labels sit at the end of each line instead of in a legend where they fit.
  - The highlighted item uses the accent colour; the others are grey.
- **Alt text.** Every image carries a sentence such as "Bar chart, Weekly sales: highest Phở bò
  1.420.000, lowest Iced Americano 300.000." Platforms show it as alt text, and the text version
  stays in the message history for later turns.

### Delivery
- `UiCaps` gains `images`, and `Card` gains `image: Option<Image>`.
  - The runtime draws only the first chart of a view, and only on channels with both `cards` and
    `images`. That card's text then leaves the chart out.
  - The PNG bytes are not saved with the view (`#[serde(skip)]`). An image without bytes in a
    prompt read back from the store means "keep the picture the message has".
  - An adapter that cannot send the picture fails the prompt, and the runtime sends the whole view
    as text, as for any failed view. Channels without `images` get today's text bars.
- **Discord.** The image goes inside the card's container as a `MediaGallery` that points to
  `attachment://chart.png`, sent with multipart. The invite link gains *Attach Files*.
  - When a view closes, the adapter reads the message once to find its file and points the
    gallery at it again, leaving `attachments` out so Discord keeps the file.
  - If Discord refuses that edit, the edit is sent again without the gallery.
- **Telegram.** Cards are now text blocks the adapter writes itself (`cards: true`).
  - A card with a chart is sent with `sendPhoto` and multipart: the text goes in the caption (up
    to 1,024 characters) and the buttons in `reply_markup`, so chart, text and choices form one
    message.
  - A longer text goes in a separate message that carries the buttons.
  - Edits try the caption first when the prompt has a picture, and switch to the text when
    Telegram says the message has none.
- **Slack.** The chart is uploaded with `files.getUploadURLExternal`, then the bytes, then
  `files.completeUploadExternal`. It is posted as its own file message, in the same thread, just
  before the card.
  - This needs the `files:write` scope.
  - Without the scope the prompt fails, the view is sent as text, and a warning names the scope.
  - An image block that refers to the file was left out, because the image block's support for
    uploaded files is less certain than file messages.
- **Console.** The PNG is written to `$XDG_RUNTIME_DIR/okbot-console/<prompt id>-chart.png` (the
  temp folder when `XDG_RUNTIME_DIR` is unset) and the console prints its path and description, so
  charts can be checked without a chat platform. Charts hold chat data: the folder must be the
  user's own (mode 0700) and a file is never replaced.
- **Google Chat and Zalo** show only images at a public URL. They keep text until a later decision
  on serving charts from `[http] public_url`, because that would put chat data behind a
  public link.

### Configuration
```toml
[charts]
images = true                                          # default false: text bars, as today
fonts = ["/usr/share/fonts/noto/NotoSansSC-Regular.otf"]  # optional extra fonts, tried in order
```
Images also need an interactive room (ADR 0015) and a platform with `images`. `okbot doctor`
checks the fonts, and warns when no room is interactive. The renderer is wired in only when
`images = true`.

The `okbot` crate gets a default feature, `charts-ja`, which embeds the Japanese font. Without it,
the binary is about 600 KB smaller and Japanese labels fall back to text.

### Limits
The renderer refuses the following and returns the error to the agent through `show`:
- values that are not finite, or larger in size than 1e15;
- labels longer than 60 characters;
- more points or series than the kind allows.

The image is never larger than 1200 × 1680 px. Rendering runs in `spawn_blocking`, with a 2 s
timeout. If rendering fails at run time, the chart is shown as text.

## Consequences
- The release binary grew from 52.4 MB to 54.3 MB, including the multipart uploads; 0.6 MB of that
  is the Japanese font, which `charts-ja` controls. The repository gains about 670 KB of font
  files.
- Discord's gateway compression moved from zstd to zlib (`flate2`, already linked), dropping
  `zstd-sys`, which had open advisories.
- `okbot-chart` is licensed `(MIT OR Apache-2.0) AND OFL-1.1`, ships the OFL text, and is allowed
  by an exception in `deny.toml`.
- Chinese text is drawn with Japanese glyph forms where they share code points, and falls back to
  text where JIS level 1 lacks the character. A `[charts] fonts` entry fixes both. Korean needs such
  a font too.
- **Tests.**
  - Layout tests snapshot a scene (rectangles and text runs with positions), not PNG bytes, so a
    tiny-skia upgrade does not churn them.
  - Coverage tests check every Vietnamese letter, every kana and a sample of kanji.
  - A random-input test checks that no label or value panics.
- **Not yet verified live:**
  - the Slack upload flow;
  - Discord keeping an attachment that an edited Components V2 message points to again;
  - the error wording Telegram uses when a message has no caption or no text to edit.
