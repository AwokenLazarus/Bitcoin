# `agpl/`: patches to `lazarus/` (AGPL-3.0)

Every patch here touches only `lazarus/`. That directory grew out of a vendored copy of
[Ratum](https://github.com/iohzrd/ratum) and stays under the **GNU Affero General Public
License v3.0** (see the repo README's [License](../../../../../README.md#license) section).
So these patches are AGPL-3.0 too: Copyright (c) 2026 Mike Moore (AwokenLazarus), full text in
[LICENSE](LICENSE). They include `lazarus/patches/datum-gateway-split-only.patch`, a patch to the
MIT-licensed C `datum_gateway` that lives in `lazarus/`. Its upstream notices are kept.

Apply each series after its `mit/` half. See [../README.md](../README.md).
