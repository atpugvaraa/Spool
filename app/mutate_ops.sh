#!/usr/bin/env bash
# Mutation harness for the semantic operation and history invariants.
#
# A test that cannot fail proves nothing. Each mutation below breaks exactly one
# load-bearing rule in `operations.rs`; the run passes only if some test in the
# suite fails. A mutation that leaves the suite green is an untested invariant
# and is the finding, not the mutation.
#
# Usage: ./mutate_ops.sh [--keep]
set -uo pipefail

FILE="src/operations.rs"
FILTER="operations::"
BACKUP="$(mktemp -t operations.rs.XXXXXX)"

if ! grep -q "MUTATION HARNESS" "$FILE"; then
  echo "error: $FILE is missing the mutation marker; refusing to run" >&2
  exit 2
fi

cp "$FILE" "$BACKUP"
restore() { cp "$BACKUP" "$FILE"; }
trap restore EXIT

passed=0
survived=0
equivalent=0

# equiv <name> <old-snippet> <new-snippet>
#
# A mutant that is *expected* to leave the suite green. It is reported
# separately from a survivor: the invariant holds, but for a reason the reader
# has to be told, and the run still fails if one of these ever starts failing.
equiv() {
  local name="$1"
  VERDICT=equivalent
  run "$name" "$2" "$3"
  unset VERDICT
}

# run <name> <old-snippet> <new-snippet>
run() {
  local name="$1" old="$2" new="$3"
  restore
  if ! python3 -c "
import sys
path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
source = open(path).read()
if source.count(old) != 1:
    print('ANCHOR-AMBIGUOUS:' + str(source.count(old)), file=sys.stderr)
    sys.exit(3)
open(path, 'w').write(source.replace(old, new, 1))
" "$FILE" "$old" "$new"; then
    echo "SKIP  $name (anchor not unique or malformed)"
    return
  fi

  local out
  out=$(cargo test --offline "$FILTER" 2>&1)

  # A failing suite ends with `error: test failed, to rerun pass`, so the
  # presence of a `test result:` line is what separates "tests ran and
  # objected" from "the mutation did not compile".
  # Read through a here-string rather than a pipe. Under `set -o pipefail`, and
  # with `grep -q` exiting the instant it matches, a large enough `$out` leaves the
  # writer killed by SIGPIPE and the pipeline reports that instead of grep's
  # result — so a run where tests failed could be scored as a survivor. See
  # `mutate_interaction.sh` for the run where that actually happened.
  if ! grep -q "^test result:" <<<"$out"; then
    echo "ERROR $name (mutation did not compile)"
    return
  fi

  if grep -q "FAILED" <<<"$out"; then
    local failed
    failed=$(grep -cE "^test operations::tests::.*FAILED" <<<"$out")
    printf 'KILL  %-56s (%s failing)\n' "$name" "$failed"
    passed=$((passed + 1))
  else
    if [ "${VERDICT:-survived}" = "equivalent" ]; then
      printf 'EQUIV  %-56s (cannot change behaviour)\n' "$name"
      equivalent=$((equivalent + 1))
    else
      printf 'SURVIVED %-54s <-- untested invariant\n' "$name"
      survived=$((survived + 1))
    fi
  fi
}

echo "== atomicity and refusals =="

run "compound undo uses forward order" \
  'ReplayDirection::Undo => members.iter().rev().copied().collect::<Vec<_>>(),' \
  'ReplayDirection::Undo => members.iter().copied().collect::<Vec<_>>(),'

run "validate skips the duplicate-name check" \
  '                return Err(OperationError::DuplicateName(rename.after.clone()));' \
  '                return Ok(());'

run "validate skips the staleness check" \
  '                return Err(OperationError::StaleEdit(rename.id.clone()));' \
  '                return Ok(());'

# This one is an EXPECTED-EQUIVALENT mutant, and keeping it documents why.
#
# Swapping the order of `apply` and `record_from` changes no behaviour, because
# `validate` is exhaustive: every rejection `apply` can produce is raised before
# the mutation point, so `apply` cannot fail once it is reached. That is the
# whole argument for having made `validate` exhaustive, so it is worth stating
# as a test rather than as prose. If a future operation adds a rejection that
# `validate` does not mirror, this mutant stops being equivalent and starts
# failing -- which is exactly when someone should be told.
equiv "execute records before applying" \
  '        apply(
            &operation,
            &mut OperationTarget::Full { document, runtime },
            ReplayDirection::Redo,
        )?;
        self.history.record_from(origin, operation);' \
  '        self.history.record_from(origin, operation.clone());
        apply(
            &operation,
            &mut OperationTarget::Full { document, runtime },
            ReplayDirection::Redo,
        )?;'

equiv "a stale rename is reported as a missing node" \
  '            ModelError::InvalidOperation(_) => Self::StaleEdit(rename.id.clone()),' \
  '            ModelError::InvalidOperation(_) => Self::MissingNode(rename.id.clone()),'

run "no-op operations are recorded anyway" \
  '        if operation.is_noop() {
            return Ok(false);
        }
        let Self {
            document, runtime, ..
        } = self;' \
  '        if false {
            return Ok(false);
        }
        let Self {
            document, runtime, ..
        } = self;'

echo
echo "== compound composition =="

run "nested compounds are not flattened" \
  '                Self::Compound(inner) => flattened.extend(inner),' \
  '                Self::Compound(inner) => flattened.push(Self::Compound(inner)),'

run "a compound is a no-op if ANY member is" \
  'Self::Compound(operations) => operations.iter().all(SemanticOperation::is_noop),' \
  'Self::Compound(operations) => operations.iter().any(SemanticOperation::is_noop),'

run "compound validation stops at the first member" \
  '            for member in operations {
                validate(member, &working, runtime)?;' \
  '            for member in operations.iter().take(1) {
                validate(member, &working, runtime)?;'

echo
echo "== gesture lifecycle =="

run "a gesture records its operations immediately" \
  '        if let Some(in_flight) = in_flight {
            in_flight.applied.push(operation);
        }
        Ok(true)' \
  '        if let Some(in_flight) = in_flight {
            in_flight.applied.push(operation.clone());
        }
        self.history.record(operation);
        Ok(true)'

run "cancelling a gesture does not reverse what it applied" \
  '        for operation in in_flight.applied.iter().rev() {' \
  '        for operation in std::iter::empty::<&SemanticOperation>() {'

run "cancelling a gesture leaves the geometry moved" \
  '        self.runtime.restore_snapshots(&in_flight.geometry);' \
  '        let _ = &in_flight.geometry;'

run "commit drops the gesture structural operations" \
  '        let mut members = in_flight.applied;' \
  '        let mut members = Vec::new();'

echo
echo "== provenance =="

run "peek_undo_origin reads the wrong stack" \
  '    pub fn peek_undo_origin(&self) -> Option<Origin> {
        self.undo.last().map(|entry| entry.origin)
    }' \
  '    pub fn peek_undo_origin(&self) -> Option<Origin> {
        self.redo.last().map(|entry| entry.origin)
    }'

run "record_from ignores the supplied origin" \
  '        self.undo.push(HistoryEntry { operation, origin });
        self.redo.clear();' \
  '        let _ = origin;
        self.undo.push(HistoryEntry { operation, origin: Origin::User });
        self.redo.clear();'

echo
echo "== replay =="

run "plan_move_nodes measures from the wrong origin" \
  '            let after = Geometry {
                position: Point {
                    x: snapshot.geometry.position.x + dx,
                    y: snapshot.geometry.position.y + dy,
                },' \
  '            let after = Geometry {
                position: Point { x: dx, y: dy },'

run "geometry_command keeps unchanged objects" \
  'fn geometry_command(runtime: &Document, snapshots: &[ObjectSnapshot]) -> DocumentCommand {
    let changes: Vec<GeometryChange> = snapshots
        .iter()
        .filter_map(|snapshot| {
            let after = runtime.geometry_of(snapshot.id)?;
            (after != snapshot.geometry).then_some(GeometryChange {' \
  'fn geometry_command(runtime: &Document, snapshots: &[ObjectSnapshot]) -> DocumentCommand {
    let changes: Vec<GeometryChange> = snapshots
        .iter()
        .filter_map(|snapshot| {
            let after = runtime.geometry_of(snapshot.id)?;
            Some(GeometryChange {'

run "redo no longer clears the redo branch" \
  '        self.undo.push(HistoryEntry { operation, origin });
        self.redo.clear();' \
  '        self.undo.push(HistoryEntry { operation, origin });'

echo
echo "== summary =="
echo "killed:      $passed"
echo "survived:    $survived     (each is an untested invariant)"
echo "equivalent:  $equivalent   (mutating these cannot change behaviour)"

if [ "${1:-}" = "--keep" ]; then
  echo "leaving the last mutation in place (--keep)"
else
  restore
  echo "source restored"
fi

[ "$survived" -eq 0 ]