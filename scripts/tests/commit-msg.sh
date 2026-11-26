#!/bin/sh
set -eu

root=$(CDPATH= cd "$(dirname "$0")/../.." && pwd)
checker=$root/.githooks/commit-msg
sandbox=$(mktemp -d "${TMPDIR:-/tmp}/slimenrf-commit-msg.XXXXXXXX")
trap 'rm -rf "$sandbox"' 0
trap 'exit 1' HUP INT TERM

# Only the owned fixture may see these configuration and identity overrides.
for name in $(git rev-parse --local-env-vars); do
    unset "$name"
done
unset GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL
export LC_ALL=C GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="$sandbox/gitconfig"
export GIT_AUTHOR_DATE=2001-01-01T00:00:00Z GIT_COMMITTER_DATE=2001-01-01T00:00:00Z
: > "$GIT_CONFIG_GLOBAL"
git init -q --template= -b fixture "$sandbox/worktree"
CDPATH= cd "$sandbox/worktree"
git config user.name 'Hook fixture'
git config user.email 'fixture@example.invalid'
git config commit.gpgsign false
git config core.autocrlf false
mkdir .githooks
git config core.hooksPath .githooks
cp "$checker" .githooks/commit-msg
chmod +x .githooks/commit-msg

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    exit 1
}

stage() {
    printf '%s\n' "$1" > payload.txt
    git add payload.txt
}

accept_commit() {
    expected=$1
    shift
    stage "$expected"
    git commit -q "$@" > .git/fixture-output 2>&1 || {
        cat .git/fixture-output >&2
        fail 'valid commit was rejected'
    }
    [ "$(git log -1 --format=%B)" = "$expected" ] || fail 'unexpected committed message'
    printf 'PASS: accepted %s\n' "${expected%%
*}"
}

failures=0
reject_commit() {
    label=$1
    diagnostic=$2
    shift 2
    stage "$label"
    old_head=$(git rev-parse HEAD)
    old_tree=$(git write-tree)
    if git commit -q "$@" > .git/fixture-output 2>&1; then
        printf 'FAIL: Git committed %s: %s\n' "$label" "$(git log -1 --format=%s)" >&2
        failures=$((failures + 1))
        return
    fi
    grep -Fq "$diagnostic" .git/fixture-output || {
        cat .git/fixture-output >&2
        fail "unexpected rejection for $label"
    }
    [ "$(git rev-parse HEAD)" = "$old_head" ] || fail "HEAD changed for $label"
    [ "$(git write-tree)" = "$old_tree" ] || fail "staged contents changed for $label"
    printf 'PASS: rejected %s without changing HEAD or staged contents\n' "$label"
}

subject_error='commit-msg: expected a Conventional Commit subject'
separator_error='commit-msg: separate the subject'
accept_commit 'docs: establish fixture' -m 'docs: establish fixture'
message='change(serial/console)!: 更新命令格式

保留已有连接设置。

BREAKING CHANGE: 移除旧命令。'
accept_commit "$message" -m "$message"
reject_commit 'freeform subject' "$subject_error" -m 'Add macOS device support and preview DMGs'
reject_commit 'merge-looking subject' "$subject_error" -m "Merge branch 'preview'"
reject_commit 'empty scope' "$subject_error" -m 'fix(): retain settings'
reject_commit 'empty description' "$subject_error" -m 'fix:   '
reject_commit 'unseparated body' "$separator_error" -m 'fix(console): retain settings
Keep the existing command defaults.'
reject_commit 'unseparated trailer' "$separator_error" -m 'fix: retain settings
BREAKING CHANGE: remove the old command.'
reject_commit 'unseparated literal comment' "$separator_error" -m 'fix: retain settings
# This line is kept when no editor runs.'
[ "$failures" -eq 0 ] || exit 1

# Preserve the real status help and verbose diff, but replace Git's initial
# blank line with the draft. Save the exact editor output to detect hook writes.
cat > .git/fixture-editor <<'EOF'
#!/bin/sh
set -eu
{
    cat .git/fixture-message
    IFS= read -r first_line
    cat
} < "$1" > .git/fixture-draft
cp .git/fixture-draft "$1"
EOF
export GIT_EDITOR='sh .git/fixture-editor'
printf '# Local reminder\r\nfix(console): 保留终端设置\r\n\r\n如实保留现有连接。\r\n' > .git/fixture-message
accept_commit 'fix(console): 保留终端设置

如实保留现有连接。'
grep -Fq '# On branch fixture' .git/fixture-draft || fail 'Git did not supply status comments'
cmp -s .git/fixture-draft .git/COMMIT_EDITMSG || fail 'checker modified the editor input'

printf 'docs(console): describe the editor\n' > .git/fixture-message
accept_commit 'docs(console): describe the editor' --verbose
grep -Fq 'diff --git a/payload.txt b/payload.txt' .git/fixture-draft || fail 'Git did not supply a verbose diff'
cmp -s .git/fixture-draft .git/COMMIT_EDITMSG || fail 'checker modified the verbose editor input'

git config core.commentChar ';'
printf '; Local reminder\ndocs: honor configured comments\n' > .git/fixture-message
accept_commit 'docs: honor configured comments'
git config --unset core.commentChar

git config commit.cleanup whitespace
printf 'docs: preserve comment text\n# This line is retained by whitespace cleanup.\n' > .git/fixture-message
reject_commit 'retained editor comment' "$separator_error"
git config --unset commit.cleanup
[ "$failures" -eq 0 ] || exit 1

# A one-off bypass permits the commit; standalone validation still reports it.
printf 'fix: retain settings\n# This is a committed body line.\n' > .git/fixture-message
git commit -q --allow-empty --no-verify --cleanup=verbatim -F .git/fixture-message
git cat-file commit HEAD > .git/fixture-object
awk 'body { print } /^$/ { body = 1 }' .git/fixture-object > .git/fixture-raw
cmp -s .git/fixture-message .git/fixture-raw || fail 'fixture did not preserve raw commit lines'
if sh .githooks/commit-msg --raw .git/fixture-raw > .git/fixture-output 2>&1; then
    fail 'raw validation ignored an unseparated committed comment'
fi
grep -Fq "$separator_error" .git/fixture-output || {
    cat .git/fixture-output >&2
    fail 'unexpected raw-message diagnostic'
}
cmp -s .git/fixture-message .git/fixture-raw || fail 'checker modified the raw input'
printf 'PASS: raw validation checked the committed lines without changing them\n'
