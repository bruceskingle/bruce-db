# bruce-util-progress

Reporting the progress of long work, and cancelling it, between a library doing the work and the app showing it: a
`Progress` the work reports to and asks whether to stop, a `Cancel` token the app keeps to stop it, and the `Cancelled`
error the work returns when it has. Std only, with no dependencies, so that it builds for ESP-IDF devices as well as
computers.

For now a package in [bruce-db](https://github.com/bruceskingle/bruce-db)'s repository, its first user; its name says it is due to move to a repository of its
own.

License
=======

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
