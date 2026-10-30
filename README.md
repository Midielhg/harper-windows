> ## This is the Windows fork
>
> A fork of [Automattic's Harper](https://github.com/automattic/harper) that
> makes **Harper Desktop work on Windows**. Upstream's desktop app targets
> macOS: the overlay was there, the Windows OS integration behind it was not.
>
> **What this fork adds**
>
> - A Windows `OsBroker` built on UI Automation: it reads the focused app's
>   text, places the underlines, follows the selection, and applies fixes.
> - **Fixes applied by pasting rather than typing.** Injected keystrokes cost
>   230–312ms *each* on a machine with a slow low-level keyboard hook, so a
>   ten-character correction took nearly five seconds. A paste is four events
>   however long the replacement is: applying a fix went from ~4,700ms to
>   19–26ms.
> - **A Grammarly-style review.** Select some text, click the blue arrow at the
>   start of the selection, and the panel lists only the mistakes inside it.
> - Underlines that **follow a window as it is dragged**, instead of being left
>   behind while the app is too busy to answer.
> - A tray icon that is **visible on a light taskbar** — the original artwork is
>   pure white, drawn for the macOS menu bar.
> - A highlighter that **stops dying** when a UAC prompt or the lock screen
>   takes the screen, and **stops freezing** when the app it is correcting hangs.
>
> **Building:** see [`CLAUDE.md`](CLAUDE.md), which also records what is still
> broken and how each problem was actually diagnosed.
>
> Everything below is upstream's README, unchanged.

---

<div id="header" align="center">
    <img src="logo.svg" width="400px" />
    <h1>Harper</h1>
</div>

[![Harper Binaries](https://github.com/automattic/harper/actions/workflows/binaries.yml/badge.svg)](https://github.com/automattic/harper/actions/workflows/binaries.yml)
[![Website](https://github.com/automattic/harper/actions/workflows/build_web.yml/badge.svg)](https://github.com/automattic/harper/actions/workflows/build_web.yml)
[![Checks](https://github.com/automattic/harper/actions/workflows/just_checks.yml/badge.svg)](https://github.com/automattic/harper/actions/workflows/just_checks.yml)
[![Crates.io](https://img.shields.io/crates/v/harper-ls)](https://crates.io/crates/harper-ls)
![NPM Version](https://img.shields.io/npm/v/harper.js)
![Downloads](https://img.shields.io/github/downloads/automattic/harper/total?label=Binary+Downloads)
![Obsidian Plugin Downloads](https://img.shields.io/github/downloads/automattic/harper-obsidian-plugin/total?label=Obsidian+Plugin+Downloads)

Harper is an English grammar checker designed to be _just right._
I created it after years of dealing with the shortcomings of the competition.

Grammarly was too expensive and too overbearing.
Its suggestions lacked context, and were often just plain _wrong_.
Not to mention: it's a privacy nightmare.
Everything you write with Grammarly is sent to their servers.
Their privacy policy claims they don't sell the data, but that doesn't mean they don't use it to train large language models and god knows what else.
Not only that, but the round-trip-time of the network request makes revising your work all the more tedious.

LanguageTool is great, if you have gigabytes of RAM to spare and are willing to download the ~16GB n-gram dataset.
Besides the memory requirements, I found LanguageTool too slow: it would take several seconds to lint even a moderate-size document.

That's why I created Harper: it is the grammar checker that fits my needs.
Not only does it take milliseconds to lint a document, take less than 1/50th of LanguageTool's memory footprint,
but it is also completely private.

Harper is even small enough to load via [WebAssembly.](https://writewithharper.com)

## Language Support

Harper currently only supports English, but the core is extensible to support other languages, so we welcome contributions that allow for other language support.

## Performance Issues

We consider long lint times bugs.
If you encounter any significant performance issues, please create an issue on the topic.

If you find a fix to any performance issue, we would appreciate the contribution.
Just please make sure to read [our contribution guidelines first.](https://writewithharper.com/docs/contributors/introduction)

## Links

- [Frequently Asked Questions](https://writewithharper.com/#faqs)
- [Obsidian Documentation](https://writewithharper.com/docs/integrations/obsidian)
- [`harper-ls` Documentation](https://writewithharper.com/docs/integrations/language-server)
- Supported Editors' Documentation
  - [Visual Studio Code](https://writewithharper.com/docs/integrations/visual-studio-code)
  - [Neovim](https://writewithharper.com/docs/integrations/neovim)
  - [Helix](https://writewithharper.com/docs/integrations/helix)
  - [Emacs](https://writewithharper.com/docs/integrations/emacs)
  - [Zed](https://writewithharper.com/docs/integrations/zed)
- [`harper.js` Documentation](https://writewithharper.com/docs/harperjs/introduction)
- [Official Discord Server](https://discord.com/invite/JBqcAaKrzQ)

## Huge Thanks

This project would not be possible without the hard work from those who [contribute](https://writewithharper.com/docs/contributors/introduction).

<a href="https://github.com/automattic/harper/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=automattic/harper" />
</a>

Harper's logo was designed by [Lukas Werner](https://lukaswerner.com/).
