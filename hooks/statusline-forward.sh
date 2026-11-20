# Paste right after `input=$(cat)` in your status-line script: forwards the status JSON to Traffic Light (plan limits).
printf '%s' "$input" | curl -s -m 1 -X POST -H 'Content-Type: application/json' --data-binary @- http://127.0.0.1:7890/status >/dev/null 2>&1 &
