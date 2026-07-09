#!/bin/sh
# THOR - NAS deploy watcher (generic template; fill in your own paths).
#
# Runs as a root scheduled task (e.g. DSM Task Scheduler, every ~5 min) so a
# non-root user can trigger a container rebuild WITHOUT sudo over SSH:
#
#   1. copy a source tarball (git archive of the thor/ crate) into the project
#      dir - on Synology use `scp -O` (DSM has no SFTP subsystem, plain scp fails
#      with "subsystem request failed"):
#        scp -O thor-src-<rev>.tar.gz <user>@<nas>:<project-dir>/
#   2. touch the trigger:
#        ssh <user>@<nas> "touch <project-dir>/deploy-requested.flag"
#
# The next tick unpacks + rebuilds + restarts; progress lands in deploy.log.
# ALWAYS validate after editing:  sh -n deploy-watcher.sh
# (a syntax error makes the watcher silently do nothing every tick: flags pile
# up and deploy.log's mtime falls behind the flag's mtime).
#
# The unpack EXCLUDES deploy/docker-compose.yml: your live compose at the
# project root carries the real network config and must never be overwritten by
# the repo's placeholder version. The data volume (data/) is not in the tarball
# at all, so the store always survives a deploy.

T=/path/to/your/thor-project
[ -f $T/deploy-requested.flag ] || exit 0
rm -f $T/deploy-requested.flag
{
  echo "START thor-deploy $(date '+%F %T') - the Rust release build takes ~10-20 min on NAS hardware; long silent windows are normal, NOT a hang"
  TAR=$(ls -t $T/thor-src-*.tar.gz 2>/dev/null | head -1)
  if [ -n "$TAR" ]; then
    echo "UNPACK $TAR"
    tar xzf "$TAR" --strip-components=1 --exclude='thor/deploy/docker-compose.yml' -C $T && echo "UNPACK_DONE"
  fi
  cd $T &&
  /usr/local/bin/docker compose build && echo "BUILD_DONE $(date '+%T')" &&
  /usr/local/bin/docker compose up -d && echo "UP_DONE $(date '+%T')"
  echo "EXIT=$?"
} > $T/deploy.log 2>&1
