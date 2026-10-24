"use strict";
// Backend policy. Durations are not tool arguments.
const INTERACTION_MS = 30000;
const EXTENDED_MS = 60000;
const OBSERVATION_MS = 30000;
function stepLimit(step) {
  // An explicit delay consumes the task budget, not an equal-length watchdog.
  if (step.do === "wait" && step.secs != null) return Infinity;
  return step.do === "navigate" || step.do === "download" || step.do === "wait" ||
    (step.do === "tab" && step.action === "new" && step.url) ? EXTENDED_MS : INTERACTION_MS;
}
module.exports = { INTERACTION_MS, EXTENDED_MS, OBSERVATION_MS, stepLimit };
