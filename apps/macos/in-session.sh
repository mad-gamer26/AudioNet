#!/bin/sh
# Runs a command inside the logged-in user's own session (as a one-off
# launchd job), from anywhere, for example over SSH: code signing needs the
# user's login keychain, which SSH sessions cannot use. No window opens and
# no password is needed; the user must be logged in on the Mac.
#
#   apps/macos/in-session.sh COMMAND [ARGS...]
#
# Prints the command's output when it finishes and exits with its status.
set -eu
uid=$(id -u)
label="org.audionet.in-session.$$"
work=$(mktemp -d -t audionet-in-session)
trap 'launchctl bootout "gui/$uid/$label" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
# The command, with the caller's directory and PATH.
{
    echo "#!/bin/sh"
    echo "cd '$(pwd)'"
    echo "export PATH='$PATH'"
    # launchd jobs do not inherit the caller's environment: pass on the
    # packaging settings that are set.
    for v in VERSION DEVELOPER_ID TEAM_ID NOTARY_PROFILE NOTARY_KEY NOTARY_KEY_ID NOTARY_ISSUER; do
        eval "val=\${$v:-}"
        [ -n "$val" ] && echo "export $v='$val'"
    done
    printf 'exec'
    for a in "$@"; do printf " '%s'" "$(printf '%s' "$a" | sed "s/'/'\\\\''/g")"; done
    echo
} > "$work/run.sh"
cat > "$work/job.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$label</string>
<key>ProgramArguments</key><array><string>/bin/sh</string><string>-c</string>
<string>/bin/sh '$work/run.sh' &gt; '$work/out.txt' 2&gt;&amp;1; echo \$? &gt; '$work/status'</string></array>
<key>RunAtLoad</key><true/>
<key>LimitLoadToSessionType</key><string>Aqua</string>
</dict></plist>
EOF
launchctl bootstrap "gui/$uid" "$work/job.plist"
while [ ! -f "$work/status" ]; do sleep 1; done
cat "$work/out.txt"
exit "$(cat "$work/status")"
