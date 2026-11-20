# rgg — mockups

Drawn at 120×32 cells. `▓…▓` marks the current match (strong highlight), `░…░` other matches (light highlight), `▶` the selected row.

## 1. Main screen, per-file mode (default)

```
┌ Search ─────────────────────────────────────┐┌ helix-core/src/doc_formatter/test.rs ─────────── match 2/9 ─┐
│ foo█                                        ││ 140 │     }                                                  │
└─────────────────────────────────────────────┘│ 141 │                                                        │
 312 files · 540 matches · done 0.048s         │ 142 │     #[test]                                            │
 [literal] [smart-case] [cs, xml]              │ 143 │     fn annotation() {                                  │
┌ Files ──────────────────────────────────────┐│ 144 │         assert_eq!(                                    │
│   docs/CONTRIBUTING.md                  (1) ││ 145 │             annotate_text("bar", false, &[Inline…      │
│   languages.toml                        (1) ││ 146 │                 "░foo░bar "                            │
│ ▶ …core/src/doc_formatter/test.rs       (9) ││ 147 │         );                                             │
│   …core/src/doc_formatter/mod.rs        (4) ││ 148 │         assert_eq!(                                    │
│   …core/src/syntax/config.rs            (2) ││ 149 │             annotate_text(                             │
│   book/src/guides/indent.md             (1) ││ 150 │                 &"▓foo▓ ".repeat(10),                  │
│   book/src/themes.md                    (1) ││ 151 │                 true,                                  │
│   book/src/command-line.md              (2) ││ 152 │                 &[InlineAnnotation::new(0, "░foo░ … │
│   helix-term/src/commands.rs           (14) ││ 153 │             ),                                         │
│   helix-term/src/ui/editor.rs           (3) ││ 154 │             "░foo░ ░foo░ ░foo░ ░foo░ \n.░foo░ ░f… │
│   helix-view/src/document.rs            (6) ││ 155 │         );                                             │
│   …                                         ││ 156 │     }                                                  │
│                                             ││ 157 │                                                        │
│                                             ││ 158 │     #[test]                                            │
│                                             ││ 159 │     fn annotation_and_overlay() {                      │
│                                             ││ 160 │         let annotations = [InlineAnnotation {         │
│                                             ││ 161 │             char_idx: 0,                               │
│                                             ││ 162 │             text: "░foo░",                             │
│                                             ││ 163 │         }];                                            │
│                                             ││ 164 │                                                        │
└─────────────────────────────────────────────┘└─────────────────────────────────────────────────────────────┘
 ↑↓ select · ^N/^P next/prev match · Enter open in VS Code · ^R regex · ^T types · Alt+W wrap · ^O settings · F1 · Esc
```

- The preview title shows the file and "match i/N". Ctrl+N/P move between matches, and the preview re-centres.
- Syntax colours aren't shown here. In the app, the preview uses the syntect theme.

## 2. Main screen, per-match mode

```
┌ Search ─────────────────────────────────────┐┌ helix-core/src/doc_formatter/test.rs:150 ──────────────────┐
│ fo+\b█                                      ││ 148 │         assert_eq!(                                    │
└─────────────────────────────────────────────┘│ 149 │             annotate_text(                             │
 312 files · 540 matches · searching…          │ 150 │                 &"▓foo▓ ".repeat(10),                  │
 [regex] [smart-case] [all types]              │ 151 │                 true,                                  │
┌ Matches ────────────────────────────────────┐│ …                                                            │
│   docs/CONTRIBUTING.md:12   with ░foo░ and  ││                                                              │
│   languages.toml:373        name = "░foo░"  ││                                                              │
│   …/doc_formatter/test.rs:146  "░foo░bar "  ││                                                              │
│ ▶ …/doc_formatter/test.rs:150  &"░foo░ ".re ││                                                              │
│   …/doc_formatter/test.rs:152  &[InlineAnn… ││                                                              │
│   …                                         ││                                                              │
└─────────────────────────────────────────────┘└─────────────────────────────────────────────────────────────┘
 ↑↓ select · Enter open in VS Code · ^R regex · ^T types · Alt+W wrap · ^O settings · F1 help · Esc quit
```

## 2b. Results from several repositories (search started in `D:\Git`)

```
┌ Files ──────────────────────────────────────┐
│─ api ───────────────────────────────────────│
│▶ src\Controllers\OrderController.cs     (3) │
│  src\Services\OrderService.cs           (1) │
│─ web ───────────────────────────────────────│
│  src\app\orders\order.component.ts      (2) │
│─ tools\scripts ─────────────────────────────│
│  deploy.ps1                             (1) │
└─────────────────────────────────────────────┘
```

Enter on `OrderController.cs` opens `D:\Git\api` in VS Code.

## 3. Invalid regex

```
┌ Search ─────────────────────────────────────┐
│ foo(█                                       │
└─────────────────────────────────────────────┘
 regex error: unclosed group                      ← red; the previous results stay visible below
 [regex] [smart-case] [cs]
```

## 4. File type picker (Ctrl+T)

```
              ┌ File types ─────────────────────────────── 2 selected ┐
              │ Filter: c█                                            │
              ├───────────────────────────────────────────────────────┤
              │   [ ] c          *.[chH], *.[chH].in, *.cats          │
              │   [ ] cmake      *.cmake, CMakeLists.txt              │
              │ ▶ [x] cs         *.cs                                 │
              │   [ ] cshtml     *.cshtml                             │
              │   [ ] css        *.css, *.scss                        │
              │   [ ] cpp        *.[ChH], *.cc, *.[ch]pp, *.[ch]xx…   │
              ├───────────────────────────────────────────────────────┤
              │ ↑↓ move · Space toggle · ^A clear all · Enter ok · Esc│
              └───────────────────────────────────────────────────────┘
```

Selected types that the filter hides (here `xml`) stay selected.

## 5. Settings (Ctrl+O)

```
              ┌ Settings ─────────────────────────────────────────────┐
              │ ▶ Search mode          ‹ literal ›                    │
              │   Case                 ‹ smart ›                      │
              │   Include hidden       ‹ off ›                        │
              │   Include ignored      ‹ off ›                        │
              │   List mode            ‹ per file ›                   │
              │   Wrap preview         ‹ off ›                        │
              │   After opening        ‹ stay open ›                  │
              │   Max file size (MB)   ‹ 10 ›                         │
              │   Max files            ‹ 10000 ›                      │
              │   Theme                ‹ base16-ocean.dark ›          │
              │   Editor command       code {root} -g {file}:{line}…  │
              ├───────────────────────────────────────────────────────┤
              │ ↑↓ move · ←→ change · Enter edit text · Esc close+save│
              └───────────────────────────────────────────────────────┘
```

Changes apply right away (the search re-runs) and are saved to `~/.rgg/config.toml` when the popup closes.
