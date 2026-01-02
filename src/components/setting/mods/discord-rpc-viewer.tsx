import { List, ListItem, ListItemText, TextField, Button } from "@mui/material";
import { useLockFn } from "ahooks";
import { forwardRef, useImperativeHandle, useState } from "react";
import { useTranslation } from "react-i18next";

import { BaseDialog, DialogRef } from "@/components/base";
import { useVerge } from "@/hooks/use-verge";
import { showNotice } from "@/services/notice-service";
import { triggerDiscordRpcReload, unloadDiscordRpc } from "@/services/cmds";

export const DiscordRpcViewer = forwardRef<DialogRef>((props, ref) => {
    const { t } = useTranslation();
    const { verge, patchVerge } = useVerge();

    const [open, setOpen] = useState(false);
    const [appId, setAppId] = useState("");

    useImperativeHandle(ref, () => ({
        open: () => {
            setOpen(true);
            setAppId(verge?.discord_app_id || "");
        },
        close: () => setOpen(false),
    }));

    const onSave = useLockFn(async () => {
        try {
            await patchVerge({
                discord_app_id: appId || undefined,
            });
            setOpen(false);
        } catch (err) {
            showNotice.error(err);
        }
    });

    return (
        <BaseDialog
            open={open}
            title={t("settings.modals.discordRpc.title")}
            contentSx={{ width: 450 }}
            okBtn={t("shared.actions.save")}
            cancelBtn={t("shared.actions.cancel")}
            onClose={() => setOpen(false)}
            onCancel={() => setOpen(false)}
            onOk={onSave}
        >
            <List>
                <ListItem sx={{ padding: "5px 2px" }}>
                    <ListItemText
                        primary={t("settings.modals.discordRpc.fields.appId")}
                        sx={{ maxWidth: "fit-content", mr: 2 }}
                    />
                    <TextField
                        autoComplete="off"
                        size="small"
                        autoCorrect="off"
                        autoCapitalize="off"
                        spellCheck="false"
                        sx={{ flex: 1 }}
                        value={appId}
                        placeholder="Default"
                        onChange={(e) => setAppId(e.target.value)}
                    />
                </ListItem>

                <ListItem sx={{ padding: "5px 2px", justifyContent: "flex-end", gap: 1 }}>
                    <Button
                        variant="outlined"
                        color="error"
                        onClick={async () => {
                            try {
                                await unloadDiscordRpc();
                                showNotice.success("Discord RPC stopped");
                            } catch (err) {
                                showNotice.error(err);
                            }
                        }}
                    >
                        Unload
                    </Button>
                    <Button
                        variant="outlined"
                        onClick={async () => {
                            try {
                                await triggerDiscordRpcReload();
                                showNotice.success("Discord RPC reloaded");
                            } catch (err) {
                                showNotice.error(err);
                            }
                        }}
                    >
                        Reload Context
                    </Button>
                </ListItem>
            </List>
        </BaseDialog>
    );
});
