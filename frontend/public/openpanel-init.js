// Product analytics for the browser console. The host must explicitly opt in;
// desktop builds and self-hosted consoles stay silent by default. The
// collector defaults to the TinyHumans OpenPanel API base when the host
// supplies no `analyticsEndpoint`.
if (
  window.__TAURI_INTERNALS__ ||
  window.OPENCOMPANY_CONFIG?.analytics !== true
) {
  // No analytics client is installed in the desktop or default deployment.
} else {
window.op = window.op || function () {
  var queue = [];
  return new Proxy(function () {
    if (arguments.length) queue.push(Array.prototype.slice.call(arguments));
  }, {
    get: function (_target, property) {
      return property === "q"
        ? queue
        : function () { queue.push([property].concat(Array.prototype.slice.call(arguments))); };
    },
    has: function (_target, property) { return property === "q"; },
  });
}();

window.op("init", {
  apiUrl: window.OPENCOMPANY_CONFIG.analyticsEndpoint || "https://panel.tinyhumans.ai/api",
  clientId: "afe8ec4e-0a6a-427a-aa22-49cbbf137d0a",
  trackScreenViews: false,
  trackOutgoingLinks: false,
  trackAttributes: false,
});

var openPanelScript = document.createElement("script");
openPanelScript.src = "https://openpanel.dev/op1.js";
openPanelScript.async = true;
document.head.appendChild(openPanelScript);
}
