import { invoke } from "@tauri-apps/api/core";
import { useManagedAuth } from "./useManagedAuth";

export interface CodeBuddyCredentialStatus {
  total_count: number;
  active_count: number;
  current_index: number;
  auto_rotation_enabled: boolean;
  rotation_count: number;
  manual_selected_id: string | null;
}

/**
 * CodeBuddy 认证 hook
 *
 * 复用通用 useManagedAuth，指定 provider 为 "codebuddy"，
 * 并提供凭证轮换、导入等 CodeBuddy 专属功能。
 */
export function useCodeBuddyAuth() {
  const managedAuth = useManagedAuth("codebuddy");

  const toggleAutoRotation = async (enabled: boolean) => {
    await invoke("codebuddy_toggle_auto_rotation", { enabled });
  };

  const importFromDirectory = async (path: string) => {
    await invoke("codebuddy_import_from_directory", { path });
  };

  const getCredentialStatus = async (): Promise<CodeBuddyCredentialStatus> => {
    return invoke<CodeBuddyCredentialStatus>("codebuddy_get_credential_status");
  };

  return {
    ...managedAuth,
    toggleAutoRotation,
    importFromDirectory,
    getCredentialStatus,
  };
}
