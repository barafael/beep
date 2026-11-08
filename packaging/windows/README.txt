b33p, a Signal client for the desktop
https://github.com/barafael/beep

Installing it
-------------
Unpack this folder anywhere and run b33p.exe. It installs itself for you alone, without
asking for admin rights: it copies itself to %LOCALAPPDATA%\b33p\b33p.exe, adds b33p to the
Start menu and to Settings -> Apps -> Installed apps, and starts from there. You can delete
this folder afterwards.

A newer b33p installs over an older one the same way (a running b33p is closed first). An
older one never replaces a newer: it starts the installed one instead, whose data may already
be in the newer format.

The program isn't signed, so Windows SmartScreen may say it "protected your PC" the first
time. Choose "More info", then "Run anyway". Only do that for a b33p.exe downloaded from the
releases page above.

On first start, b33p shows a code: on your phone, open Signal -> Settings -> Linked devices
-> Link new device, and scan it.

To run b33p from this folder without installing it, set the environment variable
B33P_NO_INSTALL to 1 before starting it.

Where things are
----------------
The program:             %LOCALAPPDATA%\b33p\b33p.exe
Your messages and keys:  %APPDATA%\barafael\b33p\data
The log:                 %LOCALAPPDATA%\barafael\b33p\data\logs
Settings:                %APPDATA%\barafael\b33p\config

The log leaves out messages, names and numbers. Attach it when you report a problem:
https://github.com/barafael/beep/issues

Removing it
-----------
1. In b33p, Settings -> Unlink this device (removes it from your phone's linked devices).
2. Settings -> Apps -> Installed apps -> b33p -> Uninstall. b33p asks whether to delete your
   messages, keys and settings too; say No to keep them for installing again later.
   (The same: %LOCALAPPDATA%\b33p\b33p.exe --uninstall)

It removes the Start menu entry, starting with Windows, the notification registration and
the program itself.

b33p is free software under the GNU Affero General Public License 3.0; see LICENSE.
It is not made or endorsed by Signal.
