# Changelog

## [0.5.1](https://github.com/flodurorg/tcomp/compare/v0.5.0...v0.5.1) (2026-09-14)


### Bug Fixes

* **server:** emit semicolon-form SGR colours in the resync dump ([7a0b837](https://github.com/flodurorg/tcomp/commit/7a0b837d8c60c4e24cfde871fd26ad03c7fec521))

## [0.5.0](https://github.com/flodurorg/tcomp/compare/v0.4.0...v0.5.0) (2026-09-14)


### Features

* **terminal:** count down restart timeout ([13181b0](https://github.com/flodurorg/tcomp/commit/13181b0bb597a4ed5696835fb9c19a2b3caf17f7))
* **terminal:** keep watch links readable before full-screen apps ([208aa32](https://github.com/flodurorg/tcomp/commit/208aa32a9b7469927cd8f82d3d18c5cdf08d86b8))
* **terminal:** offer a restart when the wrapped command exits ([5245151](https://github.com/flodurorg/tcomp/commit/5245151b3eb7d0bb37237a49a90b919648fbcb7d))
* **terminal:** report title and working directory ([c3819ec](https://github.com/flodurorg/tcomp/commit/c3819ec55cd6ca7c063b169bacec8b10f6f0b16c))
* **web:** add terminal layout choices ([0c95f75](https://github.com/flodurorg/tcomp/commit/0c95f75c22651fd10fd35f83882804d71c38af47))
* **web:** improve onscreen terminal keyboard ([6bd9193](https://github.com/flodurorg/tcomp/commit/6bd9193c33513117ec7d7e66e658372e4b2606c3))


### Bug Fixes

* **relay:** mark a session stale when its producer goes silent ([516ba56](https://github.com/flodurorg/tcomp/commit/516ba5657aec7507200af05c458fb0e8305938d8))
* **terminal:** simplify restart prompt ([31ec9ee](https://github.com/flodurorg/tcomp/commit/31ec9eebdbf8ca18e3936e8f5370931e12cc3b58))
* **web:** make ctrl a sticky modifier key ([2aa0a02](https://github.com/flodurorg/tcomp/commit/2aa0a021e7b754d8aac93e5e90a14903e1c7e37a))
* **web:** make shift a keyboard modifier ([ca416f1](https://github.com/flodurorg/tcomp/commit/ca416f1c473abad0d22aa6bfd8d0cc856812ac9b))
* **web:** style better ([026312a](https://github.com/flodurorg/tcomp/commit/026312a2221646fe2c3eb24aaf269114e47facb1))
* **web:** style ctrl combos as modifiers ([eba6211](https://github.com/flodurorg/tcomp/commit/eba621178ad440ea18d03e2d01cf6fee0c015b30))


### Documentation

* document taking over an existing terminal ([49fe7fd](https://github.com/flodurorg/tcomp/commit/49fe7fda350d316945c2bc955e89c10f279c6d4e))

## [0.4.0](https://github.com/flodurorg/tcomp/compare/v0.3.1...v0.4.0) (2026-09-14)


### Features

* **auth:** read the relay token from a file ([60e3464](https://github.com/flodurorg/tcomp/commit/60e3464bda9d2593dc918cb0fad9ac1d5a321c94))
* **nix:** install tcomp straight from the flake ([645e4e6](https://github.com/flodurorg/tcomp/commit/645e4e6d2e26ed2786d65d41f5d80857c1128eac))


### Bug Fixes

* **relay:** install a rustls provider before dialling a relay ([150975e](https://github.com/flodurorg/tcomp/commit/150975ebaa46e0cc8765f77af92a1000c7fe0a5e))

## [0.3.1](https://github.com/flodurorg/tcomp/compare/v0.3.0...v0.3.1) (2026-09-14)


### Bug Fixes

* **ci:** publish the chart only once the image is pushed ([d7c54cf](https://github.com/flodurorg/tcomp/commit/d7c54cfb3f98056c2113020d565a9736f1f69f72))

## [0.3.0](https://github.com/flodurorg/tcomp/compare/v0.2.0...v0.3.0) (2026-09-14)


### Features

* **chart:** add a Helm chart that tracks the release version ([197d86b](https://github.com/flodurorg/tcomp/commit/197d86bf526eedf7a90264bd4484d1779beda404))
* **chart:** generate a relay token when none is given ([72e320f](https://github.com/flodurorg/tcomp/commit/72e320f62a06c1105949bcfc1c6b5ba8616fcfa0))
* **chart:** route through the Gateway API as well as an Ingress ([4ab2f0a](https://github.com/flodurorg/tcomp/commit/4ab2f0aa99e8cab6b679318ae50cafe8a93f595a))
* **ci:** link the image, chart and binaries from the release notes ([66713c2](https://github.com/flodurorg/tcomp/commit/66713c2dcb091c5b7c8c6925e4658bdfaeb7b66f))
* **ci:** publish the chart to ghcr next to the image ([7b11625](https://github.com/flodurorg/tcomp/commit/7b116250a57eaf6f2236f13ca488a72a6dc219fd))


### Bug Fixes

* **chart:** stop passing serve twice to the container ([0cda3ea](https://github.com/flodurorg/tcomp/commit/0cda3ea9f5e0a1ede52dc6827bf5a004bb6e70d0))

## [0.2.0](https://github.com/flodurorg/tcomp/compare/v0.1.0...v0.2.0) (2026-09-14)


### Features

* **auth:** enforce a shared token across the relay ([9c4d6ce](https://github.com/flodurorg/tcomp/commit/9c4d6ce73ed89ddc5452d11982255fcd11e2bd07))
* **standalone:** add --bind for the embedded relay ([f83c5cd](https://github.com/flodurorg/tcomp/commit/f83c5cd1bd081659c76491a6d12621ed567b655c))


### Bug Fixes

* **ci:** keep release tags free of the component prefix ([218e7f5](https://github.com/flodurorg/tcomp/commit/218e7f5a31c29e1d70aa8a351caa22f409330e6c))
* **standalone:** honour TCOMP_PUBLIC_URL like serve does ([bffb869](https://github.com/flodurorg/tcomp/commit/bffb869b62735d605d0c1c732b857e960177b402))


### Refactors

* flatten the workspace into a root crate ([75cbfc1](https://github.com/flodurorg/tcomp/commit/75cbfc1557dbfbbfcc3dda54bc5d54553cfd6ba3))


### Documentation

* add tailscale section for standalone ([6f45472](https://github.com/flodurorg/tcomp/commit/6f4547217597b667648858afdde38a18c9d31c7d))
* drop the fidelity paragraph from the intro ([01ee8d7](https://github.com/flodurorg/tcomp/commit/01ee8d75252cc0a6e4e87351dccd6e38a5067cd7))
* lead with a demo gif, cut the README down ([95b081f](https://github.com/flodurorg/tcomp/commit/95b081fe39f374a3b4223da29d020100457f86ba))
* lead with what tcomp is for, not what it is called ([04416a4](https://github.com/flodurorg/tcomp/commit/04416a4ebe4d68047924a805f8e780a2c7854084))
* open on starting a session and carrying it with you ([2f6a594](https://github.com/flodurorg/tcomp/commit/2f6a5947c328485e2040b2420d94051994cf0716))
* restructure the README around features and parameters ([6731234](https://github.com/flodurorg/tcomp/commit/67312347a624f9f49fb91a1ca5f0ca356b4fe28f))
* tabulate session states ([3ec7e0d](https://github.com/flodurorg/tcomp/commit/3ec7e0db50f009c17005327887fde1a84b6fa0e9))
