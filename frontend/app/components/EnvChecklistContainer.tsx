// The setup wizard's environment preflight, network half: `GET /api/setup/env` on mount and
// on Retry, plus the verdict it reports back up.
//
// The verdict is the reason this is a container and not a leaf with a fetch in it: the wizard
// gates its Next button on "every required row passes", and that answer is a function of a
// response only this half has. The markup is EnvChecklistView.
import { useCallback, useEffect, useState } from "react";

import { EnvChecklistView } from "~/components/EnvChecklistView";
import { getSetupEnv } from "~/lib/api";
import type { EnvCheckRow } from "~/lib/wire/EnvCheckRow";

export function EnvChecklistContainer({
  onChange,
}: {
  /** Reports whether every *required* row passes (Next is gated on it). */
  onChange?: (allRequiredPass: boolean) => void;
}) {
  const [rows, setRows] = useState<EnvCheckRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(() => {
    setLoading(true);
    setError(null);
    getSetupEnv()
      .then((r) => setRows(r.rows))
      .catch((e: Error) => setError(e.message))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    if (rows) onChange?.(rows.every((r) => !r.required || r.ok));
  }, [rows, onChange]);

  return <EnvChecklistView rows={rows} loading={loading} error={error} onRetry={refresh} />;
}
