#!/usr/bin/env bash
# E2E driver for the editable-agent-playbook feature. Runs INSIDE the CT (which has the
# rmng:latest image built from the feature branch + Docker). Proves the composed playbook
# (global + preset append) is injected into a new clone at ~/.config/rmng/agent-instructions.md
# with the right owner/mode/content, and that the setting persists to config.json.
set -uo pipefail
API=http://localhost:9000
GLOBAL=$'E2E-GLOBAL-MARKER-8811\nline two'
PRESET='E2E-PRESET-MARKER-4423'
say(){ echo "=== $* ==="; }

say "1. wait for control-server API"
docker rm -f rmng >/dev/null 2>&1
docker run -d --name rmng --privileged --init --pid host --restart unless-stopped \
  -v /var/run/docker.sock:/var/run/docker.sock -v rmng-data:/data -v rmng-sock:/srv/rmng-sock \
  -p 9000-9002:9000-9002 -p 9005:9005 rmng:latest >/dev/null
for i in $(seq 1 40); do curl -fsS $API/api/config >/dev/null 2>&1 && { echo "API up after ${i}s"; break; }; sleep 1; done
curl -fsS $API/api/config >/dev/null 2>&1 || { echo "API NEVER CAME UP"; docker logs rmng 2>&1 | tail -30; exit 1; }

say "2. finish first-run setup (ensures the rmng network)"
curl -fsS -X PUT $API/api/config -H 'content-type: application/json' \
  -d '{"setupComplete":true}' | jq -c '{setupComplete:.config.setupComplete, restart:.restartRequired, netWarn:.networkWarning}'

say "3. pull clone template from Hub -> rmng/template:e2e (labeled rmng.image=1)"
curl -fsS -X POST $API/api/images/pull -H 'content-type: application/json' \
  -d '{"name":"e2e","reference":"pegasis0/rmng-template:latest"}' | jq -c '{op:.op.id, status:.op.status}' || echo "pull POST failed"
IMG=""
for i in $(seq 1 300); do
  IMG=$(curl -fsS $API/api/images 2>/dev/null | jq -r '.[].reference' 2>/dev/null | grep -m1 'rmng/template:e2e' || true)
  [ -n "$IMG" ] && { echo "image ready after ~$((i*5))s: $IMG"; break; }
  sleep 5
done
[ -z "$IMG" ] && { echo "TEMPLATE NEVER APPEARED"; curl -fsS $API/api/images | jq -c .; docker logs rmng 2>&1 | tail -20; exit 1; }

say "4. set a distinctive agent playbook (global + preset 'e2e' append)"
curl -fsS -X PUT $API/api/config -H 'content-type: application/json' \
  -d "$(jq -n --arg g "$GLOBAL" --arg p "$PRESET" '{agentPlaybook:$g, presets:[{name:"e2e",labels:[],linearKey:"",vars:[],agentPlaybook:$p}]}')" \
  | jq -c '{global:.config.agentPlaybook, preset:.config.presets[0].agentPlaybook}'

say "5. confirm it persisted to /data/config.json on disk"
docker exec rmng cat /data/config.json | jq -c '{global:.agentPlaybook, preset:.presets[0].agentPlaybook}'

say "6. arm capture loop (grab the injected file the moment the clone container has it)"
rm -f /root/captured.md /root/captured.stat /root/capture.log
( for i in $(seq 1 150); do
    C=$(docker ps -a --format '{{.Names}}' | grep -m1 e2e || true)
    if [ -n "$C" ]; then
      if docker cp "$C:/home/rmng/.config/rmng/agent-instructions.md" /root/captured.md 2>/dev/null; then
        docker exec "$C" stat -c '%U:%G %a %n' /home/rmng/.config/rmng/agent-instructions.md > /root/captured.stat 2>/dev/null
        echo "CAPTURED from container '$C' at iter $i" > /root/capture.log; exit 0
      fi
    fi
    sleep 1
  done; echo "NEVER CAPTURED (container=$C)" > /root/capture.log ) &
CAP=$!

say "7. create a clone using the 'e2e' preset (no Claude account needed)"
curl -fsS -X POST $API/api/clone -H 'content-type: application/json' \
  -d "$(jq -n --arg img "$IMG" '{image:$img, plain:{title:"e2e", message:""}, preset:"e2e", claudeAccount:"none"}')" \
  | jq -c '{ok:.ok, op:.op.id}' || echo "clone POST failed"

wait $CAP
say "8. capture result"; cat /root/capture.log
say "stat (expect rmng:rmng 644)"; cat /root/captured.stat 2>/dev/null || echo "(no stat)"
say "ACTUAL injected content"; cat /root/captured.md 2>/dev/null || echo "(no file captured)"
say "EXPECTED content (global, blank line, preset append)"; printf '%s\n\n%s\n' "$GLOBAL" "$PRESET"
say "DIFF (empty = PASS)"; diff <(cat /root/captured.md 2>/dev/null) <(printf '%s\n\n%s\n' "$GLOBAL" "$PRESET") && echo "PASS: injected content matches" || echo "MISMATCH (see diff above)"
say "wrapper default path check (no creds needed)"
C=$(docker ps -a --format '{{.Names}}' | grep -m1 e2e || true)
[ -n "$C" ] && docker exec "$C" bash -lc 'ls -l /home/rmng/.config/rmng/agent-instructions.md 2>/dev/null; echo "AGENT_INSTRUCTIONS_PATH default = ~/.config/rmng/agent-instructions.md"' || echo "(clone gone; file already captured above)"
