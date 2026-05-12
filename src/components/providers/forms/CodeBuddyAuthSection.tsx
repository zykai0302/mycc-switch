import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Loader2,
  LogOut,
  ExternalLink,
  Plus,
  X,
  User,
  RefreshCw,
} from "lucide-react";
import { useCodeBuddyAuth } from "./hooks/useCodeBuddyAuth";

interface CodeBuddyAuthSectionProps {
  className?: string;
  selectedAccountId?: string | null;
  onAccountSelect?: (accountId: string | null) => void;
}

export const CodeBuddyAuthSection: React.FC<CodeBuddyAuthSectionProps> = ({
  className,
  selectedAccountId,
  onAccountSelect,
}) => {
  const { t } = useTranslation();

  const {
    accounts,
    defaultAccountId,
    hasAnyAccount,
    pollingState,
    deviceCode,
    error,
    isPolling,
    isAddingAccount,
    isRemovingAccount,
    isSettingDefaultAccount,
    addAccount,
    removeAccount,
    setDefaultAccount,
    cancelAuth,
    logout,
    toggleAutoRotation,
  } = useCodeBuddyAuth();

  const [autoRotation, setAutoRotation] = React.useState(true);

  const handleAutoRotationChange = async (enabled: boolean) => {
    setAutoRotation(enabled);
    try {
      await toggleAutoRotation(enabled);
    } catch (e) {
      console.error("[CodeBuddy] Failed to toggle auto rotation:", e);
      setAutoRotation(!enabled);
    }
  };

  const handleAccountSelect = (value: string) => {
    onAccountSelect?.(value === "none" ? null : value);
  };

  const handleRemoveAccount = (accountId: string, e: React.MouseEvent) => {
    e.stopPropagation();
    e.preventDefault();
    removeAccount(accountId);
    if (selectedAccountId === accountId) {
      onAccountSelect?.(null);
    }
  };

  return (
    <div className={`space-y-4 ${className || ""}`}>
      {/* Auth status header */}
      <div className="flex items-center justify-between">
        <Label>{t("codebuddy.authStatus", { defaultValue: "认证状态" })}</Label>
        <Badge
          variant={hasAnyAccount ? "default" : "secondary"}
          className={hasAnyAccount ? "bg-green-500 hover:bg-green-600" : ""}
        >
          {hasAnyAccount
            ? t("codebuddy.credentialCount", {
                count: accounts.length,
                defaultValue: `${accounts.length} 个凭证`,
              })
            : t("codebuddy.notAuthenticated", { defaultValue: "未认证" })}
        </Badge>
      </div>

      {/* Account selector */}
      {hasAnyAccount && onAccountSelect && (
        <div className="space-y-2">
          <Label className="text-sm text-muted-foreground">
            {t("codebuddy.selectCredential", { defaultValue: "选择凭证" })}
          </Label>
          <Select
            value={selectedAccountId || "none"}
            onValueChange={handleAccountSelect}
          >
            <SelectTrigger>
              <SelectValue
                placeholder={t(
                  "codebuddy.selectCredentialPlaceholder",
                  "选择一个 CodeBuddy 凭证",
                )}
              />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="none">
                <span className="text-muted-foreground">
                  {t("codebuddy.useDefaultCredential", {
                    defaultValue: "使用默认凭证",
                  })}
                </span>
              </SelectItem>
              {accounts.map((account) => (
                <SelectItem key={account.id} value={account.id}>
                  <div className="flex items-center gap-2">
                    <User className="h-4 w-4 text-muted-foreground" />
                    <span>{account.login}</span>
                  </div>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )}

      {/* Auto-rotation toggle */}
      {hasAnyAccount && (
        <div className="flex items-center justify-between rounded-md border bg-muted/30 p-3">
          <div className="space-y-1 pr-4">
            <Label className="text-sm font-medium">
              {t("codebuddy.autoRotation", { defaultValue: "自动轮换" })}
            </Label>
            <p className="text-xs text-muted-foreground">
              {t("codebuddy.autoRotationDescription", {
                defaultValue:
                  "自动在多个凭证之间轮换，避免单个凭证使用频率过高",
              })}
            </p>
          </div>
          <Switch
            checked={autoRotation}
            onCheckedChange={handleAutoRotationChange}
            aria-label={t("codebuddy.autoRotation", {
              defaultValue: "自动轮换",
            })}
          />
        </div>
      )}

      {/* Credential list */}
      {hasAnyAccount && (
        <div className="space-y-2">
          <Label className="text-sm text-muted-foreground">
            {t("codebuddy.credentials", { defaultValue: "已登录凭证" })}
          </Label>
          <div className="space-y-1">
            {accounts.map((account) => (
              <div
                key={account.id}
                className="flex items-center justify-between p-2 rounded-md border bg-muted/30"
              >
                <div className="flex items-center gap-2">
                  <RefreshCw className="h-4 w-4 text-muted-foreground" />
                  <span className="text-sm font-medium">{account.login}</span>
                  {defaultAccountId === account.id && (
                    <Badge variant="secondary" className="text-xs">
                      {t("codebuddy.defaultCredential", {
                        defaultValue: "默认",
                      })}
                    </Badge>
                  )}
                  {selectedAccountId === account.id && (
                    <Badge variant="outline" className="text-xs">
                      {t("codebuddy.selected", { defaultValue: "已选中" })}
                    </Badge>
                  )}
                </div>
                <div className="flex items-center gap-1">
                  {defaultAccountId !== account.id && (
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-xs text-muted-foreground"
                      onClick={() => setDefaultAccount(account.id)}
                      disabled={isSettingDefaultAccount}
                    >
                      {t("codebuddy.setAsDefault", {
                        defaultValue: "设为默认",
                      })}
                    </Button>
                  )}
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7 text-muted-foreground hover:text-red-500"
                    onClick={(e) => handleRemoveAccount(account.id, e)}
                    disabled={isRemovingAccount}
                    title={t("codebuddy.removeCredential", {
                      defaultValue: "移除凭证",
                    })}
                  >
                    <X className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Not authenticated - login button */}
      {!hasAnyAccount && pollingState === "idle" && (
        <Button
          type="button"
          onClick={addAccount}
          className="w-full"
          variant="outline"
        >
          <User className="mr-2 h-4 w-4" />
          {t("codebuddy.loginWithCodeBuddy", {
            defaultValue: "使用 CodeBuddy 登录",
          })}
        </Button>
      )}

      {/* Has accounts - add more button */}
      {hasAnyAccount && pollingState === "idle" && (
        <Button
          type="button"
          onClick={addAccount}
          className="w-full"
          variant="outline"
          disabled={isAddingAccount}
        >
          <Plus className="mr-2 h-4 w-4" />
          {t("codebuddy.addAnotherCredential", {
            defaultValue: "添加其他凭证",
          })}
        </Button>
      )}

      {/* Polling state */}
      {isPolling && deviceCode && (
        <div className="space-y-3 p-4 rounded-lg border border-border bg-muted/50">
          <div className="flex items-center justify-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            {t("codebuddy.waitingForAuth", { defaultValue: "等待授权中..." })}
          </div>

          <div className="text-center">
            <p className="text-xs text-muted-foreground mb-2">
              {t("codebuddy.openAuthUrl", {
                defaultValue: "请在浏览器中完成授权：",
              })}
            </p>
            <a
              href={deviceCode.verification_uri}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1 text-sm text-blue-500 hover:underline break-all"
            >
              {deviceCode.verification_uri}
              <ExternalLink className="h-3 w-3 shrink-0" />
            </a>
          </div>

          <div className="text-center">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={cancelAuth}
            >
              {t("common.cancel", "取消")}
            </Button>
          </div>
        </div>
      )}

      {/* Error state */}
      {pollingState === "error" && error && (
        <div className="space-y-2">
          <p className="text-sm text-red-500">{error}</p>
          <div className="flex gap-2">
            <Button
              type="button"
              onClick={addAccount}
              variant="outline"
              size="sm"
            >
              {t("codebuddy.retry", { defaultValue: "重试" })}
            </Button>
            <Button
              type="button"
              onClick={cancelAuth}
              variant="ghost"
              size="sm"
            >
              {t("common.cancel", "取消")}
            </Button>
          </div>
        </div>
      )}

      {/* Logout all */}
      {hasAnyAccount && accounts.length > 1 && (
        <Button
          type="button"
          variant="outline"
          onClick={logout}
          className="w-full text-red-500 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-950"
        >
          <LogOut className="mr-2 h-4 w-4" />
          {t("codebuddy.logoutAll", { defaultValue: "注销所有凭证" })}
        </Button>
      )}
    </div>
  );
};

export default CodeBuddyAuthSection;
