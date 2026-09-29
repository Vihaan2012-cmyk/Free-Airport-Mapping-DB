A330 OANS
=========

An airport moving map (OANS) on the Headwind A330-900's navigation displays,
the captain's and the first officer's.

Turn an ND range knob anticlockwise past 10 to bring it up on that side's ND
(each side zooms on its own); keep turning for 5, 2, 1, 0.5 and 0.2 NM, and
clockwise to go back to the ND. It shows in ARC, NAV and PLAN. Click the map
for the context menu (flags, crosses, map data), drag to pan. The same actions
are bindable as H:AMDB_OANS_TOGGLE, H:AMDB_OANS_RANGE_DEC and
H:AMDB_OANS_RANGE_INC, and for the first officer's side H:AMDB_OANS_FO_TOGGLE,
H:AMDB_OANS_FO_RANGE_DEC and H:AMDB_OANS_FO_RANGE_INC.

The airport shown follows the aircraft: on the ground, or below 5,000 ft above
it, the nearest airport within 20 NM (the departure, then the destination on
approach). Pick another on the MAP DATA page.

Airport maps come from AMDB Bridge running on this computer while you fly:
https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB

Brake to vacate (BTV) -- experimental
-------------------------------------
Pick a runway and then an exit on the OANS (ND in PLAN or NAV: click the runway
end, then the exit), then press ARM BTV on the MAP DATA page of the OANS control
panel. "BTV <exit>" shows in cyan at the top of the ND. The autobrake need not
be armed; if a mode is, BTV takes it off at touchdown. It then brakes itself:
no braking while the exit is further than the aircraft would roll, then just
enough to reach the exit at the speed it is built for: 40 kt for a high-speed
exit, 10 kt for any other. BTV cannot add thrust. L:AMDB_BTV_ARM (1 = armed) is
there to bind to hardware. It lets go at that speed, on passing the exit, or when
you set the parking brake, add thrust, arm the autobrake, or clear the exit.
It is new on the A330: watch it, and brake yourself if needed.

What it changes in the Headwind A330
------------------------------------
Nothing of Headwind's is replaced or redistributed. AMDB Bridge adds a gauge
line to each ND in the aircraft's panel.cfg, and gives the two ND range knobs
their OANS positions in ModelBehaviorDefs/A339X/generated/A32NX_Interior_EFIS.xml,
keeping a backup beside each file. Turning the A330 OANS off in AMDB Bridge
(`amdb-bridge a330-oans off`) puts both files back and removes this package. A
Headwind update replaces both files, so the bridge adds the lines again the
next time it starts.

A new Community package is found by the simulator only when it starts: after
installing, restart the simulator once.

Unofficial: not made or supported by Headwind Simulations. Please send
questions and problems to the GitHub page above, not to Headwind.

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

Not affiliated with or endorsed by FlyByWire Simulations or Headwind
Simulations. NOT FOR REAL-WORLD NAVIGATION.
