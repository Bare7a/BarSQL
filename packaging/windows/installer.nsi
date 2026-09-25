Unicode true

# Keep the install folder, uninstall key and file classes as they are so upgrades install over the old copy.
# The in-app updater also sets DisplayVersion under the uninstall key.
#
#   makensis -DARG_BINARY=path\to\BarSQL.exe -DINFO_VERSION=1.1.0 -DOUTFILE=path\to\BarSQL-amd64-installer.exe \
#            installer.nsi

!ifndef ARG_BINARY
    !error "Pass -DARG_BINARY=<path to BarSQL.exe>"
!endif
!ifndef INFO_VERSION
    !error "Pass -DINFO_VERSION=<major.minor.patch>"
!endif
!ifndef OUTFILE
    !define OUTFILE "BarSQL-amd64-installer.exe"
!endif

!define INFO_PROJECTNAME "BarSQL"
!define INFO_COMPANYNAME "Bare7a"
!define INFO_PRODUCTNAME "BarSQL"
!define INFO_COPYRIGHT "(c) 2026, Bare7a"
!define PRODUCT_EXECUTABLE "${INFO_PROJECTNAME}.exe"
!define UNINST_KEY_NAME "${INFO_COMPANYNAME}${INFO_PRODUCTNAME}"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${UNINST_KEY_NAME}"
!define FILE_CLASS "SQLite Database"

!include "x64.nsh"
!include "WinVer.nsh"
!include "FileFunc.nsh"

RequestExecutionLevel admin

VIProductVersion "${INFO_VERSION}.0"
VIFileVersion    "${INFO_VERSION}.0"

VIAddVersionKey "CompanyName"     "${INFO_COMPANYNAME}"
VIAddVersionKey "FileDescription" "${INFO_PRODUCTNAME} Installer"
VIAddVersionKey "ProductVersion"  "${INFO_VERSION}"
VIAddVersionKey "FileVersion"     "${INFO_VERSION}"
VIAddVersionKey "LegalCopyright"  "${INFO_COPYRIGHT}"
VIAddVersionKey "ProductName"     "${INFO_PRODUCTNAME}"

ManifestDPIAware true

!include "MUI.nsh"

!define MUI_ICON "icon.ico"
!define MUI_UNICON "icon.ico"
!define MUI_FINISHPAGE_NOAUTOCLOSE
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Name "${INFO_PRODUCTNAME}"
OutFile "${OUTFILE}"
InstallDir "$PROGRAMFILES64\${INFO_PRODUCTNAME}"
ShowInstDetails show

!macro APP_ASSOCIATE EXT
    ReadRegStr $R0 SHELL_CONTEXT "Software\Classes\.${EXT}" ""
    WriteRegStr SHELL_CONTEXT "Software\Classes\.${EXT}" "${FILE_CLASS}_backup" "$R0"
    WriteRegStr SHELL_CONTEXT "Software\Classes\.${EXT}" "" "${FILE_CLASS}"
    WriteRegStr SHELL_CONTEXT "Software\Classes\${FILE_CLASS}" "" "SQLite Database File"
    WriteRegStr SHELL_CONTEXT "Software\Classes\${FILE_CLASS}\DefaultIcon" "" "$INSTDIR\icon.ico"
    WriteRegStr SHELL_CONTEXT "Software\Classes\${FILE_CLASS}\shell" "" "open"
    WriteRegStr SHELL_CONTEXT "Software\Classes\${FILE_CLASS}\shell\open" "" "Open with ${INFO_PRODUCTNAME}"
    WriteRegStr SHELL_CONTEXT "Software\Classes\${FILE_CLASS}\shell\open\command" "" "$INSTDIR\${PRODUCT_EXECUTABLE} $\"%1$\""
!macroend

!macro APP_UNASSOCIATE EXT
    ReadRegStr $R0 SHELL_CONTEXT "Software\Classes\.${EXT}" "${FILE_CLASS}_backup"
    WriteRegStr SHELL_CONTEXT "Software\Classes\.${EXT}" "" "$R0"
    DeleteRegKey SHELL_CONTEXT "Software\Classes\${FILE_CLASS}"
!macroend

Function .onInit
    ${IfNot} ${AtLeastWin10}
        IfSilent 0 +3
            SetErrorLevel 64
            Abort
        MessageBox MB_OK "This product is only supported on Windows 10 (Server 2016) and later."
        Quit
    ${EndIf}
    ${IfNot} ${IsNativeAMD64}
        IfSilent 0 +3
            SetErrorLevel 65
            Abort
        MessageBox MB_OK "This product can't be installed on the current Windows architecture. Supports: amd64"
        Quit
    ${EndIf}
FunctionEnd

Section
    SetShellVarContext all
    SetOutPath $INSTDIR
    File "/oname=${PRODUCT_EXECUTABLE}" "${ARG_BINARY}"
    File "icon.ico"

    CreateShortcut "$SMPROGRAMS\${INFO_PRODUCTNAME}.lnk" "$INSTDIR\${PRODUCT_EXECUTABLE}"
    CreateShortCut "$DESKTOP\${INFO_PRODUCTNAME}.lnk" "$INSTDIR\${PRODUCT_EXECUTABLE}"

    !insertmacro APP_ASSOCIATE "db"
    !insertmacro APP_ASSOCIATE "sqlite"
    !insertmacro APP_ASSOCIATE "sqlite3"
    !insertmacro APP_ASSOCIATE "s3db"
    !insertmacro APP_ASSOCIATE "sl3"

    WriteUninstaller "$INSTDIR\uninstall.exe"
    SetRegView 64
    WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "${INFO_COMPANYNAME}"
    WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${INFO_PRODUCTNAME}"
    WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${INFO_VERSION}"
    WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${PRODUCT_EXECUTABLE}"
    WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
    WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" "$\"$INSTDIR\uninstall.exe$\" /S"
    ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
    IntFmt $0 "0x%08X" $0
    WriteRegDWORD HKLM "${UNINST_KEY}" "EstimatedSize" "$0"
SectionEnd

Section "uninstall"
    SetShellVarContext all

    RMDir /r $INSTDIR

    Delete "$SMPROGRAMS\${INFO_PRODUCTNAME}.lnk"
    Delete "$DESKTOP\${INFO_PRODUCTNAME}.lnk"

    !insertmacro APP_UNASSOCIATE "db"
    !insertmacro APP_UNASSOCIATE "sqlite"
    !insertmacro APP_UNASSOCIATE "sqlite3"
    !insertmacro APP_UNASSOCIATE "s3db"
    !insertmacro APP_UNASSOCIATE "sl3"

    SetRegView 64
    DeleteRegKey HKLM "${UNINST_KEY}"
SectionEnd
