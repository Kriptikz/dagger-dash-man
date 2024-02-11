// htmx hx-swap with scroll:bottom doesn't seem to work with sse
// so here's a bit of js to autoscroll the elements
function scrollDivToBottom(scrollableDiv) {
    if (scrollableDiv) {
      scrollableDiv.scrollTop = scrollableDiv.scrollHeight;
    }
}

var observer = new MutationObserver(function(mutations) {
    mutations.forEach(function(mutation) {
      let shouldScrollCheckbox = document.getElementById('toggle-scroll');
      if (shouldScrollCheckbox && shouldScrollCheckbox.checked) {
        let scrollableDiv = document.getElementById('log-container');
        scrollDivToBottom(scrollableDiv);
      }
    });
});

var config = { childList: true, subtree: true };

observer.observe(document.getElementById('log-container'), config);

