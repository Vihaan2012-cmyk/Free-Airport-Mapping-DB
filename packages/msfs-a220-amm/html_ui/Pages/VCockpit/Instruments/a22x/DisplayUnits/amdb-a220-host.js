'use strict';
/* global BaseInstrument, Include, registerInstrument */

// Stands in for the A220's own DisplayUnits bootstrap, which this package replaces so the
// moving map can mount alongside the aircraft's displays.
//
// Everything here is the registration contract MSFS requires of a display-unit
// instrument: the same template id, the same interactivity flags, and the same two DOM
// events the A220's React displays are driven by. The aircraft's own bundle is then
// loaded unchanged, so the PFD, MFD and EICAS behave exactly as they do without us. The
// only addition is the tick handed to the map.

class AmdbA220DisplayUnits extends BaseInstrument {
    get templateID() {
        return 'DisplayUnits';
    }

    get isInteractive() {
        return true;
    }

    get IsGlassCockpit() {
        return true;
    }

    connectedCallback() {
        super.connectedCallback();
        Include.addScript('/Pages/VCockpit/Instruments/a22x/DisplayUnits/instrument.js');
    }

    Update() {
        super.Update();
        // The aircraft's displays redraw off this event; it must keep flowing untouched.
        document.dispatchEvent(new CustomEvent('update'));
        if (window.AMDB_AMM) {
            window.AMDB_AMM.tick();
        }
    }

    onInteractionEvent(event) {
        document.dispatchEvent(new CustomEvent(event));
    }
}

registerInstrument('a22x-displayunits', AmdbA220DisplayUnits);
