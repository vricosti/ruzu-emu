; SPDX-License-Identifier: GPL-3.0-or-later
;
; NSIS installer for Ruzu's Rust/GTK Windows package. Build through
; package-windows.ps1 so BINARY_SOURCE_DIR contains the GTK and
; GLib runtime beside ruzu.exe.

!ifndef PRODUCT_VERSION
  !error "PRODUCT_VERSION must be defined"
!endif
!ifndef ARCH
  !error "ARCH must be defined"
!endif
!ifndef VARIANT
  !error "VARIANT must be defined"
!endif
!ifndef BINARY_SOURCE_DIR
  !define BINARY_SOURCE_DIR "windows-package"
!endif
!ifndef OUTPUT_DIR
  !define OUTPUT_DIR "."
!endif

Unicode true
ManifestDPIAware true
RequestExecutionLevel user

!define PRODUCT_NAME "Ruzu"
!define PRODUCT_EXE "ruzu.exe"
!define PRODUCT_CLI_EXE "ruzu-cmd.exe"
!define PRODUCT_PUBLISHER "Ruzu Emulator Project"
!define PRODUCT_WEB_SITE "https://github.com/vricosti/ruzu-emu"
!define PRODUCT_PROGID "Ruzu.SwitchFile"
!define PRODUCT_DIR_REGKEY "Software\Microsoft\Windows\CurrentVersion\App Paths\${PRODUCT_EXE}"
!define PRODUCT_UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"

Name "${PRODUCT_NAME}"
OutFile "${OUTPUT_DIR}\${PRODUCT_NAME}-Windows-v${PRODUCT_VERSION}-${ARCH}-${VARIANT}-installer.exe"
SetCompressor /SOLID lzma
InstallDir "$LOCALAPPDATA\Programs\${PRODUCT_NAME}"
InstallDirRegKey HKCU "${PRODUCT_UNINST_KEY}" "InstallLocation"
ShowInstDetails show
ShowUnInstDetails show

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "nsDialogs.nsh"

!define MUI_ABORTWARNING
!define MUI_ICON "ruzu.ico"
!define MUI_UNICON "${NSISDIR}\Contrib\Graphics\Icons\modern-uninstall.ico"

!insertmacro MUI_PAGE_LICENSE "..\LICENSE"
Page custom desktopShortcutPageCreate desktopShortcutPageLeave
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\${PRODUCT_EXE}"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

Var DesktopShortcutPageDialog
Var DesktopShortcutCheckbox
Var DesktopShortcut

!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "TradChinese"
!insertmacro MUI_LANGUAGE "Danish"
!insertmacro MUI_LANGUAGE "Dutch"
!insertmacro MUI_LANGUAGE "French"
!insertmacro MUI_LANGUAGE "German"
!insertmacro MUI_LANGUAGE "Hungarian"
!insertmacro MUI_LANGUAGE "Italian"
!insertmacro MUI_LANGUAGE "Japanese"
!insertmacro MUI_LANGUAGE "Korean"
!insertmacro MUI_LANGUAGE "Lithuanian"
!insertmacro MUI_LANGUAGE "Norwegian"
!insertmacro MUI_LANGUAGE "Polish"
!insertmacro MUI_LANGUAGE "PortugueseBR"
!insertmacro MUI_LANGUAGE "Romanian"
!insertmacro MUI_LANGUAGE "Russian"
!insertmacro MUI_LANGUAGE "Spanish"
!insertmacro MUI_LANGUAGE "Swedish"
!insertmacro MUI_LANGUAGE "Turkish"
!insertmacro MUI_LANGUAGE "Vietnamese"

Function .onInit
  StrCpy $DesktopShortcut 1
  !insertmacro MUI_LANGDLL_DISPLAY
FunctionEnd

Function desktopShortcutPageCreate
  !insertmacro MUI_HEADER_TEXT "Create Desktop Shortcut" "Would you like to create a desktop shortcut?"
  nsDialogs::Create 1018
  Pop $DesktopShortcutPageDialog
  ${If} $DesktopShortcutPageDialog == error
    Abort
  ${EndIf}

  ${NSD_CreateCheckbox} 0u 0u 100% 12u "Create a desktop shortcut"
  Pop $DesktopShortcutCheckbox
  ${NSD_SetState} $DesktopShortcutCheckbox $DesktopShortcut
  nsDialogs::Show
FunctionEnd

Function desktopShortcutPageLeave
  ${NSD_GetState} $DesktopShortcutCheckbox $DesktopShortcut
FunctionEnd

Section "Ruzu" SEC_RUZU
  SectionIn RO

  IfFileExists "$INSTDIR\uninstall.exe" 0 +2
    ExecWait '"$INSTDIR\uninstall.exe" /S _?=$INSTDIR'

  SetOutPath "$INSTDIR"
  File /r "${BINARY_SOURCE_DIR}\*"

  ; Generate a relocatable loader cache after the final install path is known.
  IfFileExists "$INSTDIR\gdk-pixbuf-query-loaders.exe" 0 gdk_pixbuf_done
  IfFileExists "$INSTDIR\lib\gdk-pixbuf-2.0\2.10.0\loaders\*.dll" 0 gdk_pixbuf_done
  nsExec::ExecToLog '"$SYSDIR\cmd.exe" /D /C ""$INSTDIR\gdk-pixbuf-query-loaders.exe" "$INSTDIR\lib\gdk-pixbuf-2.0\2.10.0\loaders\*.dll" > "$INSTDIR\lib\gdk-pixbuf-2.0\2.10.0\loaders.cache""'
gdk_pixbuf_done:

  CreateDirectory "$SMPROGRAMS\${PRODUCT_NAME}"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME}.lnk" "$INSTDIR\${PRODUCT_EXE}"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\Uninstall ${PRODUCT_NAME}.lnk" "$INSTDIR\uninstall.exe"
  ${If} $DesktopShortcut == 1
    CreateShortCut "$DESKTOP\${PRODUCT_NAME}.lnk" "$INSTDIR\${PRODUCT_EXE}"
  ${EndIf}
SectionEnd

Section -Post
  WriteUninstaller "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "${PRODUCT_DIR_REGKEY}" "" "$INSTDIR\${PRODUCT_EXE}"
  WriteRegStr HKCU "${PRODUCT_DIR_REGKEY}" "Path" "$INSTDIR"

  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "DisplayIcon" "$INSTDIR\${PRODUCT_EXE}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "URLInfoAbout" "${PRODUCT_WEB_SITE}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "Publisher" "${PRODUCT_PUBLISHER}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "${PRODUCT_UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${PRODUCT_UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${PRODUCT_UNINST_KEY}" "EstimatedSize" $0

  WriteRegStr HKCU "Software\Classes\${PRODUCT_PROGID}" "" "Nintendo Switch application"
  WriteRegStr HKCU "Software\Classes\${PRODUCT_PROGID}\DefaultIcon" "" "$INSTDIR\${PRODUCT_EXE},0"
  WriteRegStr HKCU "Software\Classes\${PRODUCT_PROGID}\shell\open\command" "" '"$INSTDIR\${PRODUCT_EXE}" "%1"'
  WriteRegStr HKCU "Software\Classes\.nsp\OpenWithProgids" "${PRODUCT_PROGID}" ""
  WriteRegStr HKCU "Software\Classes\.xci\OpenWithProgids" "${PRODUCT_PROGID}" ""
  WriteRegStr HKCU "Software\Classes\.nca\OpenWithProgids" "${PRODUCT_PROGID}" ""
  WriteRegStr HKCU "Software\Classes\.nro\OpenWithProgids" "${PRODUCT_PROGID}" ""
  WriteRegStr HKCU "Software\Classes\.kip\OpenWithProgids" "${PRODUCT_PROGID}" ""

  WriteRegStr HKCU "Software\Classes\Applications\${PRODUCT_EXE}" "FriendlyAppName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "Software\Classes\Applications\${PRODUCT_EXE}\shell\open\command" "" '"$INSTDIR\${PRODUCT_EXE}" "%1"'
  WriteRegStr HKCU "Software\Classes\Applications\${PRODUCT_EXE}\SupportedTypes" ".nsp" ""
  WriteRegStr HKCU "Software\Classes\Applications\${PRODUCT_EXE}\SupportedTypes" ".xci" ""
  WriteRegStr HKCU "Software\Classes\Applications\${PRODUCT_EXE}\SupportedTypes" ".nca" ""
  WriteRegStr HKCU "Software\Classes\Applications\${PRODUCT_EXE}\SupportedTypes" ".nro" ""
  WriteRegStr HKCU "Software\Classes\Applications\${PRODUCT_EXE}\SupportedTypes" ".kip" ""
SectionEnd

Section Uninstall
  Delete "$DESKTOP\${PRODUCT_NAME}.lnk"
  RMDir /r "$SMPROGRAMS\${PRODUCT_NAME}"

  DeleteRegValue HKCU "Software\Classes\.nsp\OpenWithProgids" "${PRODUCT_PROGID}"
  DeleteRegValue HKCU "Software\Classes\.xci\OpenWithProgids" "${PRODUCT_PROGID}"
  DeleteRegValue HKCU "Software\Classes\.nca\OpenWithProgids" "${PRODUCT_PROGID}"
  DeleteRegValue HKCU "Software\Classes\.nro\OpenWithProgids" "${PRODUCT_PROGID}"
  DeleteRegValue HKCU "Software\Classes\.kip\OpenWithProgids" "${PRODUCT_PROGID}"
  DeleteRegKey HKCU "Software\Classes\${PRODUCT_PROGID}"
  DeleteRegKey HKCU "Software\Classes\Applications\${PRODUCT_EXE}"
  DeleteRegKey HKCU "${PRODUCT_UNINST_KEY}"
  DeleteRegKey HKCU "${PRODUCT_DIR_REGKEY}"

  ; Ruzu user data lives under %APPDATA%\ruzu and is deliberately preserved.
  RMDir /r "$INSTDIR"
  SetAutoClose true
SectionEnd
