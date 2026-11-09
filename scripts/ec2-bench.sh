#!/usr/bin/env bash
# Runs scripts/bench.sh on a short-lived EC2 instance, pulls the results into
# target/ec2-bench/<run>/ and deletes what it created. See docs/benchmark.md#ec2.
#
# Usage: scripts/ec2-bench.sh [--dry-run] [down]
#   Environment: INSTANCE_TYPE=i4i.4xlarge CORPORA=linux TTL=3 (hours) SPOT=1,
#   and AWS_PROFILE, AWS_REGION for the aws CLI. down deletes every run's
#   resources in the region.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

INSTANCE_TYPE=${INSTANCE_TYPE:-i4i.4xlarge}
CORPORA=${CORPORA:-linux}
TTL=${TTL:-3}
SPOT=${SPOT:-1}
TAG=fmgrep-bench
RUN=$TAG-$(date -u +%Y%m%dT%H%M%SZ)
DRY=0 CMD=run
export AWS_PAGER="" AWS_DEFAULT_OUTPUT=text

die() { echo "error: $*" >&2; exit 1; }
for a in "$@"; do
    case $a in
    --dry-run) DRY=1 ;;
    down) CMD=down ;;
    -h | --help) sed -n '2,8s/^# \{0,1\}//p' scripts/ec2-bench.sh; exit 0 ;;
    *) sed -n '2,8s/^# \{0,1\}//p' scripts/ec2-bench.sh >&2; exit 2 ;;
    esac
done
[[ $TTL =~ ^[1-9][0-9]*$ ]] || die "TTL takes whole hours"
for c in $CORPORA; do
    case $c in linux | chromium | ghc) ;; *) die "CORPORA: $c is not linux, chromium or ghc" ;; esac
done

show() {
    local a s=+
    for a; do case $a in '' | *[!A-Za-z0-9_./:=,@%+\<\>-]*) a="'$a'" ;; esac; s+=" $a"; done
    echo "$s" >&2
}
run() { show "$@"; [[ $DRY == 1 ]] || "$@"; }
# get VAR CMD...: VAR=$(CMD), or <VAR> under --dry-run.
get() {
    local __v=$1 __o
    shift
    show "$@"
    if [[ $DRY == 1 ]]; then __o="<$__v>"; else __o=$("$@"); fi
    [[ $__o != None ]] || __o=""
    printf -v "$__v" '%s' "$__o"
}

down() {
    local f="Name=tag:$1,Values=$2" ids sgs keys x i
    get ids aws ec2 describe-instances --filters "$f" Name=instance-state-name,Values=pending,running,stopping,stopped \
        --query 'Reservations[].Instances[].InstanceId'
    if [[ -n $ids ]]; then
        run aws ec2 terminate-instances --instance-ids $ids --query 'TerminatingInstances[].InstanceId'
        run aws ec2 wait instance-terminated --instance-ids $ids
    fi
    get sgs aws ec2 describe-security-groups --filters "$f" --query 'SecurityGroups[].GroupId'
    for x in $sgs; do
        # The network interface detaches a little after termination.
        for i in 1 2 3 4 5 6 7 8 9 10; do
            run aws ec2 delete-security-group --group-id "$x" && break
            [[ $i != 10 ]] || { echo "warning: $x is left; run scripts/ec2-bench.sh down later" >&2; break; }
            sleep 15
        done
    done
    get keys aws ec2 describe-key-pairs --filters "$f" --query 'KeyPairs[].KeyName'
    for x in $keys; do run aws ec2 delete-key-pair --key-name "$x"; done
}

[[ $CMD == run ]] || { down Project "$TAG"; exit 0; }

[[ -z $(git status --porcelain --untracked-files=no -- .) ]] ||
    echo "warning: uncommitted changes stay here; the instance gets HEAD ($(git rev-parse --short HEAD))" >&2
# Graviton type names have a g right after the generation digit (c7gd, m7g, i4g).
if [[ $INSTANCE_TYPE =~ ^[a-z]+[0-9]+g ]]; then ARCH=arm64; else ARCH=amd64; fi
TAGS="{Key=Project,Value=$TAG},{Key=Run,Value=$RUN}"

get T mktemp -d
trap 'set +e; down Run "$RUN"; [[ $DRY == 1 ]] || rm -rf "$T"' EXIT
trap 'exit 130' INT TERM HUP
run git archive -o "$T/src.tar.gz" HEAD .

get AMI aws ssm get-parameter --query Parameter.Value \
    --name "/aws/service/canonical/ubuntu/server/24.04/stable/current/$ARCH/hvm/ebs-gp3/ami-id"
get AZ aws ec2 describe-instance-type-offerings --location-type availability-zone \
    --filters "Name=instance-type,Values=$INSTANCE_TYPE" --query 'sort(InstanceTypeOfferings[].Location)[0]'
[[ -n $AZ ]] || die "$INSTANCE_TYPE is not offered in this region"
get MYIP curl -fsS https://checkip.amazonaws.com
run ssh-keygen -q -t ed25519 -N '' -C "$RUN" -f "$T/key"
run aws ec2 import-key-pair --key-name "$RUN" --public-key-material "fileb://$T/key.pub" \
    --tag-specifications "ResourceType=key-pair,Tags=[$TAGS]" --query KeyPairId
get SG aws ec2 create-security-group --group-name "$RUN" --description "$RUN ssh" \
    --tag-specifications "ResourceType=security-group,Tags=[$TAGS]" --query GroupId
run aws ec2 authorize-security-group-ingress --group-id "$SG" --protocol tcp --port 22 --cidr "$MYIP/32" --query Return

# A shutdown terminates the instance, so the user data bounds its life to TTL.
L=(aws ec2 run-instances --image-id "$AMI" --instance-type "$INSTANCE_TYPE" --placement "AvailabilityZone=$AZ"
    --key-name "$RUN" --security-group-ids "$SG" --instance-initiated-shutdown-behavior terminate
    --metadata-options HttpTokens=required --user-data $'#!/bin/sh\nshutdown -h +'$((TTL * 60))
    --tag-specifications "ResourceType=instance,Tags=[$TAGS]" "ResourceType=volume,Tags=[$TAGS]"
    --query 'Instances[0].InstanceId')
[[ $SPOT == 0 ]] || L+=(--instance-market-options 'MarketType=spot,SpotOptions={SpotInstanceType=one-time,InstanceInterruptionBehavior=terminate}')
get ID "${L[@]}"
run aws ec2 wait instance-running --instance-ids "$ID"
get IP aws ec2 describe-instances --instance-ids "$ID" --query 'Reservations[0].Instances[0].PublicIpAddress'
[[ -n $IP ]] || die "$ID has no public IP: a default VPC with public subnets is needed"

H=ubuntu@$IP
SSH=(-F none -i "$T/key" -o IdentitiesOnly=yes -o BatchMode=yes -o UserKnownHostsFile="$T/known_hosts"
    -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -o ServerAliveInterval=30 -o LogLevel=ERROR)
show ssh "${SSH[@]}" "$H" true
i=0
until [[ $DRY == 1 ]] || ssh "${SSH[@]}" "$H" true 2> /dev/null; do
    ((++i < 30)) || die "ssh to $IP never came up"
    sleep 10
done
run scp -q "${SSH[@]}" "$T/src.tar.gz" "$H:"

# A heredoc inside $( ) breaks bash 3.2, so read -d '' holds the instance's script.
read -r -d '' BOX << 'EOF' || true
set -euxo pipefail
# Parsed whole before it runs, so no command below reads the script as its stdin.
{
    W=/mnt/bench type=$1 rev=$2
    shift 2
    dev=$(lsblk -dnpo NAME,MODEL | awk '/Amazon EC2 NVMe Instance Storage/ {print $1; exit}')
    [[ -n $dev ]] || { echo "$type has no instance storage: pick another INSTANCE_TYPE" >&2; exit 2; }
    sudo mkfs.ext4 -q -F -E nodiscard "$dev"
    sudo mkdir -p $W && sudo mount "$dev" $W && sudo chown "$(id -u):$(id -g)" $W
    export RUSTUP_HOME=$W/rustup CARGO_HOME=$W/cargo TMPDIR=$W/tmp PATH=$W/cargo/bin:$PATH
    mkdir -p $TMPDIR $W/fmgrep $W/results
    tar xzf ~/src.tar.gz -C $W/fmgrep
    sudo DEBIAN_FRONTEND=noninteractive apt-get -yqq -o DPkg::Lock::Timeout=600 update
    sudo DEBIAN_FRONTEND=noninteractive apt-get -yqq -o DPkg::Lock::Timeout=600 install build-essential pkg-config
    curl -fsS https://sh.rustup.rs | sh -s -- -y -q --no-modify-path --profile minimal
    cargo install -q --locked ripgrep --version 15.2.0
    cargo install -q --locked elgrep --version 1.0.0
    cargo install -q --locked --git https://github.com/microsoft/tgrep --rev b1d0fc2f6245cc78f1943e5864ceeab812452404 tgrep-cli
    { echo "fmgrep $rev $type"; lscpu | grep 'Model name'; nproc; free -g; uname -r; rustc -V; rg -V; } | tee $W/results/box.txt
    cd $W/fmgrep
    rc=0
    for c in "$@"; do
        scripts/bench.sh --corpus "$c" --bench-dir $W/bench --results $W/results/results-$c.md \
            -- --rows rg,fmgrep,fmgrep-store-docs,tgrep,elgrep ||
            { s=$?; [[ $rc != 0 ]] || rc=$s; }
        cp $W/bench/trace-$c.jsonl $W/results/ || true
    done
    exit $rc
} < /dev/null
EOF

RC=0
run ssh "${SSH[@]}" "$H" bash -s "$INSTANCE_TYPE" "$(git rev-parse --short HEAD)" $CORPORA <<< "$BOX" || RC=$?
[[ $DRY == 0 ]] || printf '%s\n' "$BOX" >&2
D=target/ec2-bench/$RUN
run mkdir -p "$D"
run scp -q "${SSH[@]}" "$H:/mnt/bench/results/*" "$D/" || echo "warning: no results pulled" >&2
echo "results: $D" >&2
exit "$RC"
