# Oxidex egui Backport

This directory vendors `egui` 0.34.3 from crates.io.

Oxidex backports the non-empty IME commit behavior from upstream
[egui PR #7983](https://github.com/emilk/egui/pull/7983). Some Linux X11/XIM
stacks emit `Commit` without a usable `Preedit` event. In egui 0.34.3, the
commit was discarded when the current cursor did not match a stale saved IME
cursor, which made committed CJK text disappear after existing text.

The local change is limited to the `ImeEvent::Commit` branch in:

```text
src/widgets/text_edit/builder.rs
```

Remove this crates.io override after Oxidex upgrades to an egui release that
contains PR #7983 or equivalent commit-only IME handling.
