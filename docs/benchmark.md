# Benchmark

How to run the benchmark of fmgrep against ripgrep on a pinned corpus.

What each row and column means, and the measured results, are in
[BENCHMARKS.md](../BENCHMARKS.md#method).

## Running

```sh
scripts/bench.sh                          # GHC, cloned into target/bench/ghc
scripts/bench.sh --corpus linux --repo-path ~/src/linux
scripts/bench.sh --results bench.md -- --rows rg,fmgrep,fmgrep-store-docs
```

`scripts/bench.sh` needs `git`, `rg` and `cargo`. Without `--repo-path` it
shallow-clones the commit the corpus is pinned to and reuses the clone while
it is clean; a `--repo-path` checkout at another commit runs with a warning
that its numbers are not comparable. It builds release binaries, runs the
harness and prints its markdown, by default
`target/bench/results-<corpus>.md`, with a JSON line per query, stamped
with its kind, and per row in `target/bench/trace-<corpus>.jsonl`. Every
row prints each query's matching lines, and the harness exits 1 unless all
rows print the same lines. Arguments after `--` go to the harness;
`target/release/bench --help` lists them. The corpus checkout is
`FMGREP_CORPUS_GHC`, `FMGREP_CORPUS_LINUX` or `FMGREP_CORPUS_CHROMIUM`,
by default `target/bench/<corpus>`; the harness exits 2 when it is missing.
`--fixture` searches one file of the query set's literals instead, as a smoke
test.

## EC2

`scripts/ec2-bench.sh` runs `scripts/bench.sh` on a short-lived EC2
instance in your AWS account and copies the results back:

```sh
scripts/ec2-bench.sh --dry-run     # print every command, run none
scripts/ec2-bench.sh               # linux on an i4i.4xlarge spot instance
CORPORA="linux chromium" scripts/ec2-bench.sh
scripts/ec2-bench.sh down          # delete what an interrupted run left
```

It looks up the Ubuntu 24.04 image, creates a key pair and a security group
that admits SSH from your address only, and launches one instance that shuts
down, and so terminates, after `TTL` hours whatever happens. It copies the
committed `HEAD` there, formats the instance's NVMe storage, installs Rust,
ripgrep, tgrep and elgrep at the versions in
[BENCHMARKS.md](../BENCHMARKS.md), runs `scripts/bench.sh` with every row
for each corpus and copies `results-<corpus>.md`,
`trace-<corpus>.jsonl` and `box.txt` (the commit and the machine) to
`target/ec2-bench/<run>/`. On any exit it terminates the instance and
deletes the key pair and security group, found by their `Run` tag, and exits
with the first failing `scripts/bench.sh` status. The run is one SSH
session: if it drops, the instance still terminates after `TTL` hours, and
`down` deletes the rest.

| Variable | Default | |
|---|---|---|
| `INSTANCE_TYPE` | `i4i.4xlarge` | must have NVMe instance storage; a Graviton type gets the arm64 image |
| `CORPORA` | `linux` | any of `linux chromium ghc` |
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
it in `crates/bench/src/main.rs`.
