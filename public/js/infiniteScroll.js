// @license http://www.gnu.org/licenses/agpl-3.0.html AGPL-3.0
// SPDX-License-Identifier: AGPL-3.0-only
function getLoadMore(doc) {
    return doc.querySelector(".show-more:not(.timeline-item)");
}

function isDuplicate(item, itemClass) {
    const tweet = item.querySelector(".tweet-link");
    if (tweet == null) return false;
    const href = tweet.getAttribute("href");
    return document.querySelector(itemClass + " .tweet-link[href='" + href + "']") != null;
}

function setLoadMoreLabel(loadMore, text) {
    if (loadMore && loadMore.children[0]) {
        loadMore.children[0].text = text;
    }
}

window.onload = function () {
    const url = window.location.pathname;
    const isEngagement = /\/status\/\d+\/(retweets|quotes)/.test(url);
    const isTweet = !isEngagement && url.indexOf("/status/") !== -1;
    const containerClass = isTweet ? ".replies" : ".timeline";
    const itemClass = containerClass + " > div:not(.top-ref)";

    var html = document.querySelector("html");
    var container = document.querySelector(containerClass);
    var loading = false;

    function handleScroll(failed) {
        if (loading) return;

        if (html.scrollTop + html.clientHeight >= html.scrollHeight - 3000) {
            var loadMore = getLoadMore(document);
            if (loadMore == null) return;

            loading = true;
            setLoadMoreLabel(loadMore, "Loading...");

            var url = new URL(loadMore.children[0].href);
            url.searchParams.append("scroll", "true");

            fetch(url.toString()).then(function (response) {
                if (response.status === 204) {
                    loadMore.remove();
                    loading = false;
                    return null;
                }
                if (!response.ok) throw "error";
                return response.text();
            }).then(function (html) {
                if (html == null) return;

                var parser = new DOMParser();
                var doc = parser.parseFromString(html, "text/html");
                if (doc.querySelector(".error-panel")) throw "error";

                var items = [];
                for (var item of doc.querySelectorAll(itemClass)) {
                    if (item.className == "timeline-item show-more") continue;
                    if (isDuplicate(item, itemClass)) continue;
                    items.push(item);
                }

                var newLoadMore = getLoadMore(doc);
                if (items.length === 0 && newLoadMore == null) {
                    if (doc.querySelector(".timeline-end, .search-empty")) {
                        loadMore.remove();
                        loading = false;
                        return;
                    }
                    throw "empty";
                }

                for (var item of items) {
                    container.insertBefore(item, loadMore);
                }
                if (newLoadMore == null) {
                    loadMore.remove();
                } else {
                    loadMore.replaceWith(newLoadMore);
                }
                loading = false;
            }).catch(function (err) {
                console.warn("Something went wrong.", err);
                var next = (failed || 0) + 1;
                if (next > 3) {
                    setLoadMoreLabel(loadMore, "Error");
                    loading = false;
                    return;
                }
                setLoadMoreLabel(loadMore, "Load more");
                setTimeout(function () {
                    loading = false;
                    handleScroll(next);
                }, 1500);
            });
        }
    }

    window.addEventListener("scroll", () => handleScroll());
};
// @license-end
