# Third-Party Notices

This project is licensed under **GPL-2.0-or-later** (see [`LICENSE`](LICENSE)).

The repositories under `refs/` are not vendored and are not distributed with this
project. They are used as references:

| Project | License | Use |
|---|---|---|
| [Solid](https://github.com/solidjs/solid) | MIT | Behavioral rules and reactive algorithms. MIT is GPLv2-compatible. |
| [Leptos](https://github.com/leptos-rs/leptos) | MIT | Reactive graph, store macro, and `ScopedFuture` patterns. MIT is GPLv2-compatible. |
| [Zed / GPUI](https://github.com/zed-industries/zed) | Apache-2.0 | Architectural reference only. Apache-2.0 is compatible with GPLv3, not GPLv2; keep GPUI reference-only until the project explicitly distributes under GPLv3-compatible terms and carries the Apache notices. |

If a source file ports code from an MIT-licensed reference, add a header naming
the source file, commit, and license, and add a row here.
