A320 OANS
=========

An airport moving map (OANS) on the Fenix A320's captain navigation display.

Turn the ND range knob anticlockwise past 10 to bring it up; keep turning for
5, 2, 1, 0.5 and 0.2 NM, and clockwise to go back to the ND. It shows in ARC,
NAV and PLAN. Click the map for the context menu (flags, crosses, map data),
drag to pan. The same actions are bindable as H:AMDB_OANS_TOGGLE,
H:AMDB_OANS_RANGE_DEC and H:AMDB_OANS_RANGE_INC.

Airport maps come from a bridge running on this computer while you fly: the
"A320 OANS" program in the notification area (the standalone download starts
it with the simulator), or AMDB Bridge, which serves it too:
https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB

Brake to vacate (BTV) -- experimental
-------------------------------------
Pick a runway and then an exit on the OANS (ND in PLAN or NAV: click the runway
end, then the exit), then press ARM BTV on the MAP DATA page of the OANS control
panel. "BTV <exit>" shows in cyan at the top of the ND. The autobrake need not
be armed; if a mode is, BTV presses it off at touchdown. It then brakes itself:
no braking while the exit is further than the aircraft would roll, then just
enough to reach the exit at 10 kt. The line turns green with the distance to
go. BTV cannot add thrust: an exit further than the rollout reaches is taxied
to as usual. L:AMDB_BTV_ARM (1 = armed) is there to bind to hardware.

It lets go at 10 kt, on passing the exit, or when you set the parking brake,
add thrust, arm the autobrake, or clear the exit, and disarms itself after each
rollout. It has been flown in test runs, not yet in many landings: watch it,
and brake yourself if needed.

What it changes in the Fenix
----------------------------
Nothing of Fenix's is replaced or redistributed. AMDB Bridge adds two gauge
lines to the Fenix's panel.cfg, draws the captain ND at 1536 x 1536 pixels
rather than 768 (sharper text; Fenix's ND scales itself to it), and gives the
captain's ND range knob its OANS
positions in FNX32X_Interior.xml (Cockpit_Behavior.xml in MSFS 2024), keeping
a backup beside each file. Uninstalling the A320 OANS, or turning it off in AMDB
Bridge (`amdb-bridge a320-oans off`), puts both files back and removes this
package. A Fenix update replaces both files, so the bridge adds the lines again
the next time it starts.

Unofficial: not made or supported by Fenix Simulations. Please send questions
and problems to the GitHub page above, not to Fenix.

Credits and licence
-------------------
The display is FlyByWire Simulations' A380X OANS, used as they wrote it:
  https://github.com/flybywiresim/aircraft
  (commit 2baa2b35eadaf4c78e172ce41bbe6b40b4aeafb2, 2026-09-20)
Copyright (c) FlyByWire Simulations and its contributors.

None of FlyByWire's fonts or images are included. The display font is B612,
the typeface Airbus commissioned for cockpit displays, Copyright 2012 The B612
Project Authors, under the SIL Open Font License 1.1 (LICENSE-B612-font.txt).
The flag and cross symbols were drawn for this package.

Because it contains FlyByWire's code, this package is licensed under the GNU
General Public License, version 3 (LICENSE.txt), unlike the rest of AMDB
Bridge, which is MIT. The source for everything in it, and the script that
builds it from FlyByWire's source at the commit above, is in tools/fenix-oans
in the AMDB Bridge repository.

Not affiliated with or endorsed by FlyByWire Simulations or Fenix Simulations.
NOT FOR REAL-WORLD NAVIGATION.
