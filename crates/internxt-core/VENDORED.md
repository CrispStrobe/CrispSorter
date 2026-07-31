# Vendoring record

This directory is a local MIT-licensed test oracle copied from
[`Bebbssos/internxt-core-rust`](https://github.com/Bebbssos/internxt-core-rust)
at revision `b474edae67a53bb5cfefd108998e5b6d89251e43`.

It is used only by `crisp-internxt-native` tests for independent crypto and
protocol cross-checks. It is not the production Internxt implementation.
The dependency was changed to disable reqwest defaults and enable its rustls
backend; the upstream source and MIT license are otherwise preserved.
