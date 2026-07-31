#!/usr/bin/env bash
# The whole migration story against real services, end to end: extract a live
# ArcGIS feature service with an audit log, load it into ptolemy, dual-run a
# delta, commit the delta, and prove the result serves Esri clients through
# ptolemy's own FeatureServer facade.
#
#   SERVICE_URL          the FeatureServer to migrate (default: Esri's public
#                        Wildfire sample, read-only shared infrastructure)
#   PTOLEMY_URL          a running ptolemy, e.g. http://localhost:3000
#   VERNE_PTOLEMY_TOKEN  a bearer token with the editor or admin role
#   OUT                  working directory (default: ./migration-demo)
#
#   --force-ops   mutate a copy of the full extraction before the delta, so the
#                 delta demonstrably carries one insert, one update and one
#                 delete even when the live service has not changed
#
# The load creates the datasets, so ptolemy must not already hold datasets with
# the service's layer names: point it at a scratch instance.
set -euo pipefail

SERVICE_URL="${SERVICE_URL:-https://sampleserver6.arcgisonline.com/arcgis/rest/services/Wildfire/FeatureServer}"
OUT="${OUT:-./migration-demo}"
: "${PTOLEMY_URL:?set PTOLEMY_URL to a running ptolemy}"
: "${VERNE_PTOLEMY_TOKEN:?set VERNE_PTOLEMY_TOKEN to an editor bearer token}"

FORCE_OPS=false
[[ "${1:-}" == "--force-ops" ]] && FORCE_OPS=true

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
verne() { cargo run -q --release --manifest-path "$ROOT/Cargo.toml" -p verne-cli -- "$@"; }

step() { printf '\n== %s\n' "$*"; }

step "checking $PTOLEMY_URL has no datasets in the way"
python3 - "$PTOLEMY_URL" <<'PY'
import json, sys, urllib.request
with urllib.request.urlopen(sys.argv[1] + "/arcgis/rest/services?f=json") as resp:
    services = [s["name"] for s in json.load(resp)["services"]]
if services:
    sys.exit(f"ptolemy already serves {services}; the demo load would collide, use a scratch instance")
PY

step "full extraction of $SERVICE_URL"
verne extract "$SERVICE_URL" --out "$OUT/full" --operator "migration-demo" >/dev/null
echo "wrote $OUT/full (see the extraction log in its sidecar for what was carried and what was lost)"

step "loading the full extraction into ptolemy"
verne load "$OUT/full" --ptolemy "$PTOLEMY_URL"

BASIS="$OUT/full"
if $FORCE_OPS; then
    step "mutating a copy of the extraction so the delta carries all three op kinds"
    rm -rf "$OUT/prev-mutated"
    cp -r "$OUT/full" "$OUT/prev-mutated"
    python3 - "$OUT/prev-mutated" <<'PY'
import json, os, sys
# one property tweaked (becomes an update), one object id faked (its real oid
# becomes an insert, the fake one a delete); counts stay balanced
base = sys.argv[1]
sidecar = json.load(open(os.path.join(base, "sidecar.json")))
plan = next(p for p in sidecar["datasets"] if p.get("features") and p.get("object_id_field"))
oid_field = plan["object_id_field"]
path = os.path.join(base, plan["features"])
lines = open(path).read().splitlines()
out, tweaked, reoided = [], False, False
for line in lines:
    f = json.loads(line)
    oid = f["properties"].get(oid_field)
    if not tweaked and oid is not None:
        f["properties"]["__demo"] = "forced update"
        tweaked = True
    elif not reoided and oid is not None:
        f["properties"][oid_field] = 99999999
        reoided = True
    out.append(json.dumps(f))
open(path, "w").write("\n".join(out) + "\n")
print(f"mutated {plan['dataset']['name']}: expect 1 insert, 1 update, 1 delete there")
PY
    BASIS="$OUT/prev-mutated"
fi

step "delta extraction since the previous one (Esri stays live, only changes come down)"
rm -rf "$OUT/delta"
verne extract "$SERVICE_URL" --out "$OUT/delta" --operator "migration-demo" --since "$BASIS" \
    | grep -oE "\| [^|]+ \| feature collection \| [0-9]+ inserted[^|]*" || true

step "committing the delta onto the datasets the first load created"
verne load "$OUT/delta" --ptolemy "$PTOLEMY_URL"

step "verifying ptolemy's FeatureServer facade serves the migrated state"
python3 - "$OUT" "$PTOLEMY_URL" <<'PY'
import json, os, sys, urllib.parse, urllib.request
out, base = sys.argv[1], sys.argv[2]
full = json.load(open(os.path.join(out, "full", "sidecar.json")))
delta = json.load(open(os.path.join(out, "delta", "sidecar.json")))

def ops(plan, root):
    if not plan or not plan.get("features"):
        return []
    path = os.path.join(out, root, plan["features"])
    return [json.loads(l) for l in open(path) if l.strip()]

failed = False
for plan in full["datasets"]:
    name = plan["dataset"]["name"]
    expected = len(ops(plan, "full"))
    for op in ops(next((p for p in delta["datasets"] if p["dataset"]["name"] == name), None), "delta"):
        kind = op.get("type", "insert")
        expected += {"insert": 1, "delete": -1}.get(kind, 0)
    url = f"{base}/arcgis/rest/services/{urllib.parse.quote(name)}/FeatureServer/0/query" \
          "?where=1%3D1&returnCountOnly=true&f=json"
    with urllib.request.urlopen(url) as resp:
        served = json.load(resp).get("count")
    ok = served == expected
    failed |= not ok
    print(f"  {name}: extracted {expected}, facade serves {served} {'OK' if ok else 'MISMATCH'}")
sys.exit(1 if failed else 0)
PY

step "done"
cat <<DONE
Every Esri client keeps working against ptolemy:
  QGIS:          add an ArcGIS REST Server connection at $PTOLEMY_URL/arcgis/rest/services
  ArcGIS JS API: point a FeatureLayer at any service URL the catalog lists
Every load above is a versioned changeset; the extraction logs in
$OUT/full and $OUT/delta name everything carried, approximated or left behind.
DONE
