import { useMutation, useQueryClient } from "@tanstack/react-query";
import { announce } from "../lib/live";
import { plural, shown } from "../lib/text";
import { post } from "./client";
import type { Status, StatusChange } from "./types";

export const STATUS_WORD: Record<Status, string> = { resolved: "resolved", ignored: "ignored", unresolved: "reopened" };

/**
 * Changes the status of issues and says so. `changed` takes over refreshing
 * the list, so the list can let the rows that no longer match fade first.
 */
export function useStatusChange(changed?: (change: StatusChange) => void) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (change: StatusChange) => post<undefined>("/issues/status", change),
    onSuccess: (_, change) => {
      const n = change.issues.length;
      const where = change.release ? ` in ${shown(change.release)}` : "";
      announce(`${n} ${plural(n, "issue")} ${STATUS_WORD[change.status]}${where}`);
      if (changed) changed(change);
      else void qc.invalidateQueries({ queryKey: ["issues"] });
    },
    onError: (e) => {
      announce(`Davy did not change the status: ${e.message}`);
      void qc.invalidateQueries({ queryKey: ["issues"] });
    },
    onSettled: () => qc.invalidateQueries({ queryKey: ["issue"] }),
  });
}
