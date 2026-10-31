import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface UnmuteMicrophoneWhileRecordingProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const UnmuteMicrophoneWhileRecording: React.FC<UnmuteMicrophoneWhileRecordingProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const enabled = getSetting("unmute_microphone_while_recording") ?? false;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(value) =>
          updateSetting("unmute_microphone_while_recording", value)
        }
        isUpdating={isUpdating("unmute_microphone_while_recording")}
        label={t("settings.debug.unmuteMicrophoneWhileRecording.label")}
        description={t("settings.debug.unmuteMicrophoneWhileRecording.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  });
