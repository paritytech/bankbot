# Bankbot

The benchmarking bot.

Still in very heavy development, see
[#1](https://github.com/paritytech/bankbot/issues/1) to get an idea of the
current status.


## Overview

The bot uses [tide](https://github.com/http-rs/tide) and
[tide-github](https://github.com/paritytech/tide-github) to receive Github
webhooks for any comment on a Pull Request. If the comment begins with the
magic keyword (e.g. `/bench-bot`) a job will be created and put on the queue.

Multiple benchmark nodes can each pull from the queue over HTTP and execute the
job. Although in principle multiple benchmark nodes are supported, the peer
discovery (whether through configuration, DNS, etc) has not been decided on and
hence not implemented yet.

The job itself consist of a [rhai](https://rhai.rs/) script executed in a clean
checkout of the PR branch, provided with a nice API to execute cargo commands,
commit & push files and leave comments on Github.


### Building

You need a recent version of Rust, at least 1.56. I personally use the
equivalent of `nix-shell -p rustup gcc pkg-config openssl` with the latest
stable rust.

`cargo build --release`


## Usage

```sh
$ bankbot --help
bankbot 0.1.0
The benchmarking bot

USAGE:
    bankbot [OPTIONS] --webhook-secret <webhook-secret>

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
    -a, --address <address>                  Address to listen on [env: ADDRESS=]  [default: 127.0.0.1]
    -c, --command-prefix <command-prefix>    Bot command prefix [env: COMMAND_PREFIX=]  [default: /benchbot]
    -l, --log-level <log-level>              Log level [env: LOG_LEVEL=]  [default: info]
    -p, --port <port>                        Port to listen on [env: PORT=]  [default: 3000]
    -r, --repos-root <repos-root>            Repositories root working directory [env: REPOS_ROOT=]  [default: ./repos]
    -w, --webhook-secret <webhook-secret>    Github Webhook secret [env: WEBHOOK_SECRET]
```
