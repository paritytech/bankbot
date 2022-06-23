# CI Script

Simply automate your CI needs with the powers of the CI Scripting Language.

There are currently two ways of running CI scripts:
  * Run the [Github Webhook Reactor]("#github-webhook-reactor") and post a
    `/bot-command` comment in an issue or PR;
  * Use the [Command line tool]("#from-the-command-line") and run scripts directly from
    the command line.

In any case the parts of the CI script standard library that talk to external API's, like
the methods allowing you to create pull requests on GitHub, often require some kind of
credentials. If you want to run a script that uses these API's directly yourself you're
going to need to [acquire these](#acquiring-github-credentials). The GitLab Runners expose
environmental variables that make it easy to call CI-scripts directly from
`.gitlab-ci.yaml` without worrying about authentication 🚀.

*Both of these ways make it easy to trigger these CI scripts on specific events*, such as
when a PR is submitted or a commit is pushed.

We're still in very heavy development, see
[#1](https://github.com/paritytech/ci-script/issues/1) to get an idea of the current
status. Until version 1.0 there will probably still be significant changes in the API, but
we can help you keep up-to-date.

More documentation will be coming soon.

## Examples

### Hello, world

```rhai
// Clone a repo, say hello, and issue a PR for it
let message = `
# Hello

As you can see, backticks allow multiline strings.
`;

let repo = github::clone("koenw/ci-script", "master");
repo.branch("bla");
repo.write("hello.md", message);
repo.push("bla", "say-hello")
repo.create_pr("Say Hello", "Please just let me say hello, but in more words", "say-hello",
"master");
```

### Custom Repository Sync

```rhai
// Sync a subdirtory of our repository to (the root of) another repository

let target_repo = github::clone("koenw/substrate-node-template", "main");

// `REPO` is a global variable that contains the repository that triggered our script (if
applicable)
for f in REPO.ls-files("bin/node-template-update")
  .map(|entry| entry.path) {
    let new_path = f.strip_prefix("bin/node-template");
    target_repo.write(new_path, REPO.read(f));
}
```

### Automatic Rust FMT PR's

```rhai
REPO.branch("auto-fmt");

cargo "fmt"

let changed_files = REPO.status().changed() + REPO.status().added();
if changed_files.len() > 0 {
  for f in changed_files {
    REPO.add(f);
  }
  REPO.commit('Automatic `cargo fmt`');
  REPO.push("auto-fmt", "auto-fmt");
  REPO.create_pr('Apply `Cargo fmt`', "This is the PR body", "auto-fmt", "master");
}
```

## Executing scripts

### From the Command Line

So I was looking for something shorter than ci-script as the actual tool name, let me know
if `cis` is too much please (I'm still asking around).

```sh
❯ cis --help
ci-scripts 0.1.0
Run CI scripts, like from a CI/CD job

USAGE:
    cis [OPTIONS] <script> --github-app-id <github-app-id> --github-app-key <github-app-key> --github-name <github-name> --github-owner <github-owner> [script-args]...

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
        --clone-dir <clone-dir>              Path to the directory where the script can clone repositories to [env:
                                             CLONE_DIR=]  [default: /tmp]
        --github-app-id <github-app-id>      Github App ID [env: GITHUB_APP_ID=]
        --github-app-key <github-app-key>    Github App key [env: GITHUB_APP_KEY]
        --github-name <github-name>          Name of the upstream Github repository [env: GITHUB_NAME=]
        --github-owner <github-owner>        Owner of the upstream Github repository [env: GITHUB_OWNER=]
    -l, --log-level <log-level>              Log level [env: LOG_LEVEL=]  [default: info]
        --repo <repo>                        Path to the repository [env: REPO=]  [default: ./]

ARGS:
    <script>            Path to the script to execute relative to the root of the script's repository [env: SCRIPT=]
    <script-args>...    Arguments to pass to the script [env: SCRIPT_ARGS=
```

### GitHub Webhook Reactor

The GitHub Webhook Reactor<sup>or Receiver, Router, Runner :shrug: :)</sup> uses
[tide](https://github.com/http-rs/tide) and
[tide-github](https://github.com/paritytech/tide-github) to receive Github webhooks for
any comment on a Pull Request. If the comment begins with the magic keyword (e.g.
`/magic-bot`) a job will be created and put on the queue.

Multiple benchmark nodes can each pull from the queue over HTTP and execute the job.
Although in principle multiple benchmark nodes are supported, the peer discovery (whether
through configuration, DNS, etc) has not been decided on and hence not implemented yet.

The job itself clones the repository and executes the script in
`.github/<magic-keyword>/first_argument.rhai` if the bot is invoked with `/magic-keyword
first_argumenet`.

#### Usage

```sh
❯ cis-gh-reactor --help
ci-script 0.1.0
Simply automate your CI needs with the powers of the CI Scripting Language

USAGE:
    cis-gh-reactor [OPTIONS] --app-id <app-id> --app-key <app-key> --webhook-secret <webhook-secret>

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
    -a, --address <address>                  Address to listen on [env: ADDRESS=]  [default: 127.0.0.1]
        --app-id <app-id>                    Github App ID [env: APP_ID=]
        --app-key <app-key>                  Github App key [env: APP_KEY]
    -c, --command-prefix <command-prefix>    Bot command prefix [env: COMMAND_PREFIX=]  [default: /benchbot]
    -l, --log-level <log-level>              Log level [env: LOG_LEVEL=]  [default: info]
    -p, --port <port>                        Port to listen on [env: PORT=]  [default: 3000]
    -r, --repos-root <repos-root>            Repositories root working directory [env: REPOS_ROOT=]  [default: ./repos]
    -w, --webhook-secret <webhook-secret>    Github Webhook secret [env: WEBHOOK_SECRET]
```

## Building

You need a recent version of Rust, at least 1.56. I personally use the equivalent of
`nix-shell -p rustup gcc pkg-config openssl` with the latest stable rust.

`cargo build --release ci-script`
`cargo build --release cis-gh-reactor`
