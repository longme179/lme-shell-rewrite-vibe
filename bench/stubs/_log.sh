#!/usr/bin/env bash
# Stub tool call logger. Writes "tool arg1 arg2 …" lines to $LME_STUB_LOG.
printf '%s\n' "$1 ${*:2}" >> "${LME_STUB_LOG:-/tmp/lme-stub-calls.log}"
