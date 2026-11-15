/*
 * SPDX-FileCopyrightText: 2026 Sven Shi
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

"use client";

import { TriangleAlertIcon } from "lucide-react";

import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { WEBUI } from "@/lib/i18n";
import { useI18n } from "@/lib/i18n/provider";
import { useAppStore } from "@/lib/store";
import type { PluginInstance } from "@/lib/types";

type ProviderDegradedNoticeProps = {
  plugin: PluginInstance;
};

/**
 * Shown in the plugin detail drawer when the provider's last snapshot build
 * could not load every configured rule file. Lists the degraded files and
 * points at the reload action as the recovery path.
 */
export function ProviderDegradedNotice({ plugin }: ProviderDegradedNoticeProps) {
  const { t } = useI18n();
  const providerState = useAppStore(
    (state) => state.providerStates[plugin.name],
  );

  if (
    plugin.type !== "provider" ||
    providerState?.availability !== "ready" ||
    providerState.status !== "degraded"
  ) {
    return null;
  }

  return (
    <Card className="mt-4 border-warning/40 bg-warning/5">
      <CardHeader className="flex flex-row items-start justify-between gap-3">
        <div className="flex min-w-0 flex-col gap-1">
          <CardTitle className="flex items-center gap-2 text-sm text-warning">
            <TriangleAlertIcon className="size-4 shrink-0" />
            {t(WEBUI.plugins.providerDegradedBadge)}
          </CardTitle>
          <CardDescription>
            {t(WEBUI.plugins.providerDegradedHint)}
          </CardDescription>
        </div>
      </CardHeader>
      {providerState.degradedFiles.length > 0 ? (
        <CardContent className="flex flex-col gap-1 text-xs">
          <p className="text-muted-foreground">
            {t(WEBUI.plugins.providerDegradedFilesTitle)}
          </p>
          <ul className="flex flex-col gap-0.5 font-mono text-warning">
            {providerState.degradedFiles.map((file) => (
              <li key={file} className="break-all">
                {file}
              </li>
            ))}
          </ul>
        </CardContent>
      ) : null}
    </Card>
  );
}
