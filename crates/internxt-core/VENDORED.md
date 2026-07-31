# Vendoring record

This directory is a local MIT-licensed vendor of
[`Bebbssos/internxt-core-rust`](https://github.com/Bebbssos/internxt-core-rust)
at revision `b474edae67a53bb5cfefd108998e5b6d89251e43`.

The source is kept local so CrispSorter controls the 0.x API and can make the
mobile transport choice explicit. The dependency was changed to disable
reqwest defaults and enable its rustls backend; the upstream source and MIT
license are otherwise preserved.
