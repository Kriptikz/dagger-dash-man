function toggleAutoScroll() {
    var checkbox = document.getElementById('toggle-scroll');
    var scrollableDiv = document.getElementById('log-container');

    if (checkbox.checked) {
        scrollableDiv.scrollTop = scrollableDiv.scrollHeight;
    }
}

window.onload = function() {
    if (document.getElementById('toggle-scroll').checked) {
        toggleAutoScroll();
    }
};

var observer = new MutationObserver(function(mutations) {
    mutations.forEach(function(mutation) {
        if (document.getElementById('toggle-scroll').checked) {
            toggleAutoScroll();
        }
    });
});

var config = { childList: true, subtree: true };

observer.observe(document.getElementById('log-container'), config);

