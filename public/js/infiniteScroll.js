// @license http://www.gnu.org/licenses/agpl-3.0.html AGPL-3.0
// SPDX-License-Identifier: AGPL-3.0-only

function insertBeforeLast(node, elem) {
    node.insertBefore(elem, node.childNodes[node.childNodes.length - 2]);
}

function getLoadMore(doc) {
    return doc.querySelector(".show-more:not(.timeline-item)");
}

function getHrefs(selector) {
    return new Set([...document.querySelectorAll(selector)].map(el => el.getAttribute("href")));
}

function getTweetId(item) {
    const m = item.querySelector(".tweet-link")?.getAttribute("href")?.match(/\/status\/(\d+)/);
    return m ? m[1] : "";
}

function isDuplicate(item, hrefs) {
    return hrefs.has(item.querySelector(".tweet-link")?.getAttribute("href"));
}

function setLoadMoreLabel(loadMore, text) {
    if (loadMore && loadMore.children[0]) {
        loadMore.children[0].text = text;
    }
}

const GAP = 10;

class Masonry {
    constructor(container) {
        this.container = container;
        const colSizes = {
            small:  w => Math.max(130, w * 0.11),
            medium: w => Math.max(190, Math.min(350, w * 0.22)),
            large:  w => Math.max(350, Math.min(480, w * 0.22)),
        };
        const size = container.dataset.colSize || "medium";
        this._targetWidth = colSizes[size] || colSizes.medium;
        this.colHeights = [];
        this.colCounts = [];
        this.colCount = 0;
        this._lastWidth = 0;
        this._colWidthCache = 0;
        this._items = [];
        this._revealTimer = null;
        this.container.classList.add("masonry-active");

        let resizeTimer;
        window.addEventListener("resize", () => {
            clearTimeout(resizeTimer);
            resizeTimer = setTimeout(() => this._rebuild(), 50);
        });

        let syncTimer;
        this._observer = window.ResizeObserver ? new ResizeObserver(() => {
            clearTimeout(syncTimer);
            syncTimer = setTimeout(() => this.syncHeights(), 100);
        }) : null;

        this._rebuild();
    }

    _revealAll() {
        clearTimeout(this._revealTimer);
        for (const item of this._items) item.classList.add("masonry-visible");
        if (!this.container.parentElement) return;
        for (const el of this.container.parentElement.querySelectorAll(":scope > .show-more, :scope > .top-ref, :scope > .timeline-footer"))
            el.classList.add("masonry-visible");
    }

    _pickCol() {
        return this.colHeights.reduce((min, h, i) => {
            const m = this.colHeights[min];
            return (h < m || (h === m && this.colCounts[i] < this.colCounts[min])) ? i : min;
        }, 0);
    }

    _position(items, heights, colWidth) {
        for (let i = 0; i < items.length; i++) {
            const col = this._pickCol();
            items[i].style.left = `${col * (colWidth + GAP)}px`;
            items[i].style.top = `${this.colHeights[col]}px`;
            this.colHeights[col] += heights[i] + GAP;
            this.colCounts[col]++;
        }
        this.container.style.height = `${Math.max(0, ...this.colHeights)}px`;
    }

    _place(items, heights, n, colWidth) {
        this.colHeights = new Array(n).fill(0);
        this.colCounts = new Array(n).fill(0);
        this.colCount = n;
        this._position(items, heights, colWidth);
    }

    _rebuild() {
        const w = this.container.clientWidth;
        const n = Math.max(1, Math.floor(w / this._targetWidth(w)));
        if (n === this.colCount && w === this._lastWidth) return;

        const isFirst = this.colCount === 0;

        if (isFirst) {
            this._items = [...this.container.querySelectorAll(".timeline-item")];
        }

        this._items.sort((a, b) => {
            const idA = getTweetId(a), idB = getTweetId(b);
            if (idA.length !== idB.length) return idB.length - idA.length;
            return idB < idA ? -1 : idB > idA ? 1 : 0;
        });

        const colWidth = this._colWidthCache = Math.floor((w - GAP * (n - 1)) / n);
        for (const item of this._items) item.style.width = `${colWidth}px`;

        this._place(this._items, this._items.map(item => item.offsetHeight), n, colWidth);
        this._lastWidth = w;

        if (isFirst) {
            if (this._observer) this._items.forEach(item => this._observer.observe(item));
            const hasUnloaded = this._items.some(item =>
                [...item.querySelectorAll("img")].some(img => !img.complete));
            if (hasUnloaded) {
                this._revealTimer = setTimeout(() => this._revealAll(), 1000);
            } else {
                this._revealAll();
            }
        }
    }

    syncHeights() {
        this._place(this._items, this._items.map(item => item.offsetHeight), this.colCount, this._colWidthCache);
        this._revealAll();
    }

    addAll(newItems) {
        if (!newItems.length) return;
        const colWidth = this._colWidthCache;

        for (const item of newItems) {
            item.style.width = `${colWidth}px`;
            this.container.appendChild(item);
        }

        this._position(newItems, newItems.map(item => item.offsetHeight), colWidth);
        this._items.push(...newItems);

        if (this._observer) newItems.forEach(item => this._observer.observe(item));
    }
}

window.onload = function () {
    const path = window.location.pathname;
    const isEngagement = /\/status\/\d+\/(retweets|quotes)/.test(path);
    const isTweet = !isEngagement && path.indexOf("/status/") !== -1;
    const containerClass = isTweet ? ".replies" : ".timeline";
    const itemClass = containerClass + " > div:not(.top-ref)";
    const html = document.documentElement;
    const container = document.querySelector(containerClass);
    const masonryEl = container?.querySelector(".gallery-masonry");
    const masonry = masonryEl ? new Masonry(masonryEl) : null;
    let loading = false;

    function handleScroll(failed) {
        if (loading) return;
        if (html.scrollTop + html.clientHeight < html.scrollHeight - 3000) return;

        const loadMore = getLoadMore(document);
        if (!loadMore) return;

        loading = true;
        setLoadMoreLabel(loadMore, "Loading...");

        const url = new URL(loadMore.children[0].href);
        url.searchParams.append("scroll", "true");

        fetch(url.toString()).then(function (response) {
            if (response.status === 204) {
                loadMore.remove();
                loading = false;
                return null;
            }
            if (!response.ok) throw "error";
            return response.text();
        }).then(function (htmlText) {
            if (htmlText == null) return;

            const doc = new DOMParser().parseFromString(htmlText, "text/html");
            if (doc.querySelector(".error-panel")) throw "error";

            if (masonry) {
                masonry.syncHeights();
                const newMasonry = doc.querySelector(".gallery-masonry");
                const knownHrefs = getHrefs(".gallery-masonry .tweet-link");
                const newItems = newMasonry
                    ? [...newMasonry.querySelectorAll(".timeline-item")].filter(item => !isDuplicate(item, knownHrefs))
                    : [];
                masonry.addAll(newItems);

                const newLoadMore = getLoadMore(doc);
                if (newItems.length === 0 && newLoadMore == null) {
                    if (doc.querySelector(".timeline-end, .timeline-none, .search-empty")) {
                        loadMore.remove();
                        loading = false;
                        return;
                    }
                    throw "empty";
                }
                if (newLoadMore == null) {
                    loadMore.remove();
                } else {
                    loadMore.replaceWith(newLoadMore);
                    newLoadMore.classList.add("masonry-visible");
                }
                loading = false;
                return;
            }

            const knownHrefs = getHrefs(`${itemClass} .tweet-link`);
            const items = [];
            for (const item of doc.querySelectorAll(itemClass)) {
                if (item.className === "timeline-item show-more") continue;
                if (isDuplicate(item, knownHrefs)) continue;
                items.push(item);
            }

            const newLoadMore = getLoadMore(doc);
            if (items.length === 0 && newLoadMore == null) {
                if (doc.querySelector(".timeline-end, .timeline-none, .search-empty")) {
                    loadMore.remove();
                    loading = false;
                    return;
                }
                throw "empty";
            }

            for (const item of items) {
                if (isTweet) {
                    container.appendChild(item);
                } else {
                    container.insertBefore(item, loadMore);
                }
            }
            if (newLoadMore == null) {
                loadMore.remove();
            } else {
                loadMore.replaceWith(newLoadMore);
            }
            loading = false;
        }).catch(function (err) {
            console.warn("Something went wrong.", err);
            const next = (failed || 0) + 1;
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

    window.addEventListener("scroll", () => handleScroll());
};
// @license-end
