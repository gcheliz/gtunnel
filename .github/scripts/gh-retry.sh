# shellcheck shell=bash
# Sourced by the release workflow's steps: `. .github/scripts/gh-retry.sh`.
#
# GitHub's API answers a 5xx now and then. A step that runs each `gh` call once
# under `set -e` turns one of those into a failed release train, so every call in
# the release path goes through these helpers instead.
#
# Only a transient failure is retried: output that names an HTTP 5xx, a reset
# connection or a timeout. Anything else, a 4xx included, fails at once with the
# command's own output and exit code. The command's stdout reaches the caller
# untouched, so `x=$(retry gh ...)` captures exactly what `gh` printed; every
# note this file adds goes to stderr.
#
# RETRY_WAITS is the pause before each rerun, in seconds: "5 15" is three
# attempts. Tests shorten it; the workflow leaves it unset.

_retry_transient='HTTP 5[0-9][0-9]|connection reset|timed out|timeout'

# Runs "$@" once. Leaves its stdout and stderr in two files named by
# $_retry_out and $_retry_err, and its exit code in $_retry_status. Callers copy
# the names into locals at once: a nested call reuses these globals.
_retry_once() {
  _retry_out=$(mktemp)
  _retry_err=$(mktemp)
  if "$@" >"${_retry_out}" 2>"${_retry_err}"; then
    _retry_status=0
  else
    _retry_status=$?
  fi
}

# _retry_emit <out> <err>: hands an attempt's output to the caller, removes it.
_retry_emit() {
  cat "$1"
  cat "$2" >&2
  rm -f "$1" "$2"
}

# _retry_is_transient <out> <err>
_retry_is_transient() {
  cat "$1" "$2" | grep -qiE "${_retry_transient}"
}

# _retry_note <what> <attempt> <max> <wait> <err>: says why a rerun is coming.
_retry_note() {
  echo "::warning::'$1' hit a transient error (attempt $2 of $3); retrying in $4s" >&2
  sed 's/^/  /' "$5" >&2
}

# retry <command> [args...]
retry() {
  local -a waits
  read -r -a waits <<< "${RETRY_WAITS:-5 15}"
  local attempt=1 max=$(( ${#waits[@]} + 1 )) out err status
  while :; do
    _retry_once "$@"
    out=${_retry_out} err=${_retry_err} status=${_retry_status}
    if [ "${status}" -eq 0 ] || ! _retry_is_transient "${out}" "${err}" || [ "${attempt}" -ge "${max}" ]; then
      if [ "${status}" -ne 0 ] && [ "${attempt}" -gt 1 ]; then
        echo "retry: '$1 ${2:-}' failed after ${attempt} attempts" >&2
      fi
      _retry_emit "${out}" "${err}"
      return "${status}"
    fi
    _retry_note "$1 ${2:-}" "${attempt}" "${max}" "${waits[attempt - 1]}" "${err}"
    rm -f "${out}" "${err}"
    sleep "${waits[attempt - 1]}"
    attempt=$(( attempt + 1 ))
  done
}

# release_state <tag>: prints `draft`, `published` or `none`. Only gh's own
# "release not found" means none; any other failure is retried and then fails.
release_state() {
  local tag=$1 err state status
  err=$(mktemp)
  if state=$(retry gh release view "${tag}" --repo "${GITHUB_REPOSITORY}" --json isDraft -q .isDraft 2>"${err}"); then
    rm -f "${err}"
    case "${state}" in
      true) echo draft ;;
      false) echo published ;;
      *) echo "release_state: unexpected isDraft '${state}' for ${tag}" >&2; return 1 ;;
    esac
    return 0
  else
    status=$?
  fi
  if grep -q 'release not found' "${err}"; then
    rm -f "${err}"
    echo none
    return 0
  fi
  cat "${err}" >&2
  rm -f "${err}"
  return "${status}"
}

# release_delete <tag> [gh release delete args...]: a delete that failed but
# left no release behind did its job, since GitHub may act before it answers.
release_delete() {
  local tag=$1 status state
  shift
  if retry gh release delete "${tag}" "$@"; then
    return 0
  else
    status=$?
  fi
  state=$(release_state "${tag}") || return "${status}"
  if [ "${state}" = "none" ]; then
    echo "${tag} is gone; the failed delete is treated as done" >&2
    return 0
  fi
  return "${status}"
}

# release_create_draft <tag> [gh release create args...]: GitHub may create the
# draft before it answers 5xx, and a second create would add a second draft of
# the same tag. So after every failure the tag is viewed first, a draft counts
# as success, and only a transient failure with no draft is retried.
release_create_draft() {
  local tag=$1
  shift
  local -a waits
  read -r -a waits <<< "${RETRY_WAITS:-5 15}"
  local attempt=1 max=$(( ${#waits[@]} + 1 )) out err status state
  while :; do
    _retry_once gh release create "${tag}" --draft "$@"
    out=${_retry_out} err=${_retry_err} status=${_retry_status}
    if [ "${status}" -eq 0 ]; then
      _retry_emit "${out}" "${err}"
      return 0
    fi
    if state=$(release_state "${tag}") && [ "${state}" = "draft" ]; then
      cat "${err}" >&2
      rm -f "${out}" "${err}"
      echo "::notice::creating ${tag} reported a failure, but its draft exists; treated as created" >&2
      return 0
    fi
    if ! _retry_is_transient "${out}" "${err}" || [ "${attempt}" -ge "${max}" ]; then
      _retry_emit "${out}" "${err}"
      return "${status}"
    fi
    _retry_note "gh release create" "${attempt}" "${max}" "${waits[attempt - 1]}" "${err}"
    rm -f "${out}" "${err}"
    sleep "${waits[attempt - 1]}"
    attempt=$(( attempt + 1 ))
  done
}
