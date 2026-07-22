document.getElementById("send").addEventListener("click", () => {
  const status = document.getElementById("status");
  status.textContent = "Sending…";
  chrome.runtime.sendMessage({ type: "send-active" }, (resp) => {
    if (chrome.runtime.lastError) {
      status.textContent = chrome.runtime.lastError.message;
      return;
    }
    status.textContent = resp?.ok
      ? "Sent. The watcher will process the item."
      : resp?.error || "Failed";
  });
});
