#!/bin/bash
task_root="$(cd -- "$(dirname -- "$0")" && pwd)"
/bin/bash "$task_root/scripts/start-mac-binary.sh"
task_status=$?
echo
read -r -p "Press Return to close this window. " task_answer
exit "$task_status"
