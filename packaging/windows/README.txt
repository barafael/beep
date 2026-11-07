b33p, a Signal client for the desktop
https://github.com/barafael/beep

Starting it
-----------
Unpack this folder anywhere, for example into your Documents, and run b33p.exe.

The program isn't signed, so Windows SmartScreen may say it "protected your PC" the first
time. Choose "More info", then "Run anyway". Only do that for a b33p.exe downloaded from the
releases page above.

On first start, b33p shows a code: on your phone, open Signal -> Settings -> Linked devices
-> Link new device, and scan it.

Where things are
----------------
Your messages and keys:  %APPDATA%\barafael\b33p\data
The log:                 %LOCALAPPDATA%\barafael\b33p\data\logs
Settings:                %APPDATA%\barafael\b33p\config

The log leaves out messages, names and numbers. Attach it when you report a problem:
https://github.com/barafael/beep/issues

Removing it
-----------
1. In b33p, Settings -> Unlink this device (removes it from your phone's linked devices).
2. Quit b33p, then run from this folder:   b33p.exe --unregister
   (stops it starting with Windows, and removes its notification registration).
3. Delete this folder and the folders listed above.

b33p is free software under the GNU Affero General Public License 3.0; see LICENSE.
It is not made or endorsed by Signal.
