function toggleAutoScroll() {
    var checkbox = document.getElementById('toggleScroll');
    var scrollableDiv = document.getElementById('log-container');

    if (checkbox.checked) {
        // Scroll to the bottom
        console.log("SCROLL CHECKED");
        scrollableDiv.scrollTop = scrollableDiv.scrollHeight;
    }
    // If the checkbox is unchecked, do nothing
}

// Optional: Automatically scroll to the bottom if the checkbox is initially checked
window.onload = function() {
    if (document.getElementById('toggleScroll').checked) {
        toggleAutoScroll();
    }
};

// Optional: Keep scrolling to the bottom if new content is added and the checkbox is checked
var observer = new MutationObserver(function(mutations) {
    mutations.forEach(function(mutation) {
        if (document.getElementById('toggleScroll').checked) {
            toggleAutoScroll();
        }
    });
});

var config = { childList: true, subtree: true };

observer.observe(document.getElementById('log-container'), config);

