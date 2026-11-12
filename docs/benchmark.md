# Benchmark

How to run the benchmark of fmgrep against ripgrep on a pinned corpus.

What each row and column means, and the measured results, are in
[BENCHMARKS.md](../BENCHMARKS.md#method).

## Running

```sh
scripts/bench.sh                          # GHC, cloned into target/bench/ghc
scripts/bench.sh --corpus linux --repo-path ~/src/linux
scripts/bench.sh --corpus aosp --repo-path ~/aosp   # synced first, see AOSP
scripts/bench.sh --results bench.md -- --rows rg,fmgrep,fmgrep-store-docs
scripts/bench.sh -- --rows rg,fmgrep,fmgrep-direct  # a process per query, no server
```

`scripts/bench.sh` needs `git`, `rg` and `cargo`. Without `--repo-path` it
shallow-clones the commit the corpus is pinned to and reuses the clone while
it is clean; a `--repo-path` checkout at another commit runs with a warning
that its numbers are not comparable. It builds release binaries, runs the
harness and prints its markdown, by default
`target/bench/results-<corpus>.md`, with a JSON line per query, stamped
with its kind, and per row in `target/bench/trace-<corpus>.jsonl`. Every
row searches the checkout in place, and nothing is written under it:
indexes and tool state go to temporary directories; watchman keeps
watching it, as under [File monitor](#file-monitor). Every row first runs
the set's first query once as a warm-up, neither timed, nor kept, nor
counted in the query footprint or RSS, then times every query. Every row
prints each query's matching lines, and the harness exits 1 if an fmgrep
row prints other lines than rg from a file both searched; a tgrep or
elgrep row that does is noted under the table, and files only some rows
searched are listed in the results. Arguments after `--` go to the
harness;
`target/release/bench --help` lists them. The corpus checkout is
`FMGREP_CORPUS_GHC`, `FMGREP_CORPUS_LINUX`, `FMGREP_CORPUS_CHROMIUM` or
`FMGREP_CORPUS_AOSP`, by default `target/bench/<corpus>`; the harness exits
2 when it is missing.
`--fixture` searches one file of the query set's literals instead, as a smoke
test.

## File monitor

fmgrep does not call git. `fmgrep index` asks a running watchman (it
never starts one) to watch the tree and keeps a token from it in
`docs.journal`. On every query without a server and when a server starts,
a load of 50,000 indexed files or more asks watchman what changed since
that token; a smaller index, or one without a token, stats every indexed
file ([Staleness](index-format.md#staleness)). `fmgrep index` waits at most
5 s for watchman, so a big tree that watchman is still crawling gets no
token, and every load of it stats every file.

Before the harness runs, `scripts/bench.sh` therefore passes the checkout
to `scripts/fsmonitor.sh`, which has watchman watch it (`watchman
watch-project`, which starts watchman if it is not running) and waits until
watchman has crawled it, so the crawl is over before any `fmgrep index`. It
prints how long the crawl took. Without watchman, or when the watch fails,
it warns, and fmgrep stats every file. fmgrep runs with its defaults, so
on linux, chromium and aosp the fmgrep rows other than `--store-docs`
(whose index holds the documents and checks no tree) ask watchman at each
load, and on ghc (about 27,000 files) they stat every file. The watch
covers an AOSP tree too.
To do the same by hand, from the top of the checkout (the query below
puts `$PWD` into a JSON string as it is, so a path holding `"` or `\`
needs them escaped):

```sh
watchman watch-project .
# returns once watchman has crawled the tree:
watchman -j <<< "[\"query\", \"$PWD\", {\"expression\": [\"false\"], \"sync_timeout\": 600000}]"
```

`watchman watch-del .` removes the watch. `scripts/ec2-bench.sh` installs
watchman on the instance.

## EC2

`scripts/ec2-bench.sh` runs `scripts/bench.sh` on a short-lived EC2
instance in your AWS account and copies the results back:

```sh
scripts/ec2-bench.sh --dry-run     # print every command, run none
scripts/ec2-bench.sh               # linux on an i4i.4xlarge spot instance
CORPORA="linux chromium" scripts/ec2-bench.sh
CORPORA=aosp scripts/ec2-bench.sh  # syncs AOSP on the instance first
scripts/ec2-bench.sh down          # delete what an interrupted run left
```

It looks up the Ubuntu 24.04 image, creates a key pair and a security group
that admits SSH from your address only, and launches one instance that shuts
down, and so terminates, after `TTL` hours whatever happens. It copies the
committed `HEAD` there, formats the instance's NVMe storage, installs Rust,
watchman (a pinned prebuilt release on x86_64, else the Ubuntu package),
ripgrep, tgrep and elgrep at the versions in
[BENCHMARKS.md](../BENCHMARKS.md), for `aosp` syncs the tree as under
[AOSP](#aosp) with the repo launcher at v2.72 (about 11 GB to download),
runs `scripts/bench.sh` with every row for each corpus and copies
`results-<corpus>.md`, `trace-<corpus>.jsonl` and `box.txt` (the commit and
the machine) to `target/ec2-bench/<run>/`. On any exit it terminates the
instance and deletes the key pair and security group, found by their `Run`
tag, and exits with the first failing `scripts/bench.sh` status. The run is
one SSH session: if it drops, the instance still terminates after `TTL`
hours, and `down` deletes the rest.

| Variable | Default | |
|---|---|---|
| `INSTANCE_TYPE` | `i4i.4xlarge` | must have NVMe instance storage, about 100 GB of it and the default's 128 GB of memory for `aosp` (the elgrep index build peaks near 60 GB); a Graviton type gets the arm64 image |
| `CORPORA` | `linux` | any of `linux chromium ghc aosp` |
| `TTL` | `3` | hours until the instance terminates itself |
| `SPOT` | `1` | `0` launches on demand, when spot capacity is short |

`AWS_PROFILE` and `AWS_REGION` pick the account and region, as for any
`aws` command. It needs the AWS CLI v2, `ssh`, `git`, `curl` and a default
VPC in the region. The caller needs `ec2:DescribeInstances`,
`ec2:DescribeInstanceTypeOfferings`, `ec2:DescribeSecurityGroups`,
`ec2:DescribeKeyPairs`, `ec2:RunInstances`, `ec2:ImportKeyPair`,
`ec2:CreateSecurityGroup`, `ec2:CreateTags`,
`ec2:AuthorizeSecurityGroupIngress`, `ec2:TerminateInstances`,
`ec2:DeleteSecurityGroup`, `ec2:DeleteKeyPair` and `ssm:GetParameter` on
`arn:aws:ssm:*::parameter/aws/service/canonical/*`; the creating actions can
be limited with `aws:RequestTag/Project` on the instance, volume, key-pair
and security-group resources, and the others with
`aws:ResourceTag/Project`, both `fmgrep-bench`. A first spot launch in an
account may also need `iam:CreateServiceLinkedRole` for the EC2 Spot role.
On an i4i.4xlarge, linux and chromium together took under 0.6
instance-hours, tool builds and the sync included: about
$1 or less at on-demand prices, less on spot. `down` deletes every instance,
security group and key pair tagged `Project=fmgrep-bench` in the region,
another run's included.

## AOSP

The `aosp` corpus is the Android Open Source Project's platform source at
release tag `android-17.0.0_r1`: about a thousand git projects that the
[repo](https://gerrit.googlesource.com/git-repo) tool assembles from the
platform manifest. Projects of prebuilt binaries are left out: `prebuilts/`
and the other paths matching the `EXCLUDE` pattern below. `scripts/bench.sh`
does not clone it; sync it once, then pass the tree with `--repo-path` or
`FMGREP_CORPUS_AOSP`:

```sh
EXCLUDE='(^|/)prebuilts?(/|$)|[-_]prebuilts?$|Prebuilt$|-kernel$|^external/chromium-webview$|^device/google/cuttlefish_vmm$'
mkdir ~/aosp && cd ~/aosp
repo init --depth=1 --no-partial-clone --no-clone-bundle --repo-rev=v2.72 -b android-17.0.0_r1 \
    -u https://android.googlesource.com/platform/manifest.git
repo list -a -p | grep -Ev "$EXCLUDE" | xargs repo sync -c --no-tags -j8
```

The synced tree takes about 48 GB of disk: 1.3 million files, of which
fmgrep indexes 19 GB of text, 79% of it under `external/`. A run needs
about 30 GB more for the largest row index (rows run one at a time).

## tgrep (optional)

The tgrep row runs Microsoft's indexed-search
[tgrep](https://github.com/microsoft/tgrep): `FMGREP_BENCH_TGREP_BIN`, else the
first `tgrep` on `PATH`, provided `tgrep index --help` and `tgrep serve --help`
show `--index-path` and `--no-watch`. Otherwise the row is skipped, with the
reason on stderr and in the results. CI does not install it. Build it from
source: `cargo install --locked --git https://github.com/microsoft/tgrep
tgrep-cli`, or clone that repository and `cargo build --release`. Do not
`cargo install tgrep`: the crates.io crate named `tgrep` is an unrelated grep
clone without an index.

## elgrep (optional)

The elgrep row runs [elgrep](https://github.com/eliosai/sngram)'s `eg`:
`FMGREP_BENCH_ELGREP_BIN`, else the first `eg` on `PATH`, provided
`eg --version` starts `elgrep (eg) ` and `eg-indexd` sits beside it.
Otherwise the row is skipped, with the reason on stderr and in the results.
Install it with `cargo install --locked elgrep --version 1.0.0`. The
harness starts its own `eg-indexd` and keeps the index and the daemon's
state in the row's temporary directory, so it never touches an index of
yours, and stops the daemon when the row ends. The index answers only on
Linux, so the default rows include elgrep only there; elsewhere
`--rows elgrep` runs a row in which every query is a scan, as its note says.

## Queries

Each corpus's queries and pinned commit are in
`crates/bench/queries/<corpus>.json`; `/.../` entries are regexes and
`A AND B` entries intersections. To add a corpus, add a query file and name
it in `crates/bench/src/corpus.rs`.

## Comparing index configurations

To compare index configurations, build and serve each index yourself and
give the harness one `--server [LABEL=]HOST:PORT` per server. Each adds a
row `fmgrep@LABEL` that runs the queries through `fmgrep --server` against
that server and leaves it running; the harness first checks with
`--status` that the server holds the checkout's files. Such a row prints
`-` in the index and memory columns, and discard the first run after the
servers start, which is cold:

```sh
cargo build --release -p fmgrep
FMGREP_SHARD_BYTES=100MB target/release/fmgrep index ~/chromium --output /tmp/chromium100
FMGREP_SHARD_BYTES=250MB target/release/fmgrep index ~/chromium --output /tmp/chromium250
target/release/fmgrep serve /tmp/chromium100 --listen 127.0.0.1:7100 &
target/release/fmgrep serve /tmp/chromium250 --listen 127.0.0.1:7250 &
scripts/bench.sh --corpus chromium --repo-path ~/chromium -- --rows rg \
    --server 100mb=127.0.0.1:7100 --server 250mb=127.0.0.1:7250
```
