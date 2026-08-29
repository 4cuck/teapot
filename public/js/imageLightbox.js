(() => {
  let gallery = [];
  let activeIndex = 0;

  const dialog = document.createElement("dialog");
  dialog.className = "image-lightbox";
  dialog.setAttribute("aria-label", "Image viewer");
  dialog.innerHTML = `
    <div class="image-lightbox-frame">
      <button class="image-lightbox-close" type="button" aria-label="Close image viewer">×</button>
      <button class="image-lightbox-nav image-lightbox-prev" type="button" aria-label="Previous image">‹</button>
      <img class="image-lightbox-image" alt="">
      <button class="image-lightbox-nav image-lightbox-next" type="button" aria-label="Next image">›</button>
      <div class="image-lightbox-footer">
        <span class="image-lightbox-caption"></span>
        <a class="image-lightbox-original" target="_blank" rel="noopener">Open original</a>
      </div>
    </div>`;
  document.body.append(dialog);

  const image = dialog.querySelector(".image-lightbox-image");
  const caption = dialog.querySelector(".image-lightbox-caption");
  const original = dialog.querySelector(".image-lightbox-original");
  const previous = dialog.querySelector(".image-lightbox-prev");
  const next = dialog.querySelector(".image-lightbox-next");

  function show(index) {
    activeIndex = (index + gallery.length) % gallery.length;
    const link = gallery[activeIndex];
    const thumbnail = link.querySelector("img");
    image.src = link.href;
    image.alt = thumbnail?.alt || "";
    caption.textContent = thumbnail?.alt || `${activeIndex + 1} of ${gallery.length}`;
    original.href = link.href;
    previous.hidden = gallery.length < 2;
    next.hidden = gallery.length < 2;
  }

  document.addEventListener("click", (event) => {
    const link = event.target.closest?.("a.still-image");
    if (!link || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();

    const container = link.closest(".attachments");
    gallery = [...(container?.querySelectorAll("a.still-image") || [link])];
    show(Math.max(0, gallery.indexOf(link)));
    if (!dialog.open) dialog.showModal();
  });

  dialog.querySelector(".image-lightbox-close").addEventListener("click", () => dialog.close());
  previous.addEventListener("click", () => show(activeIndex - 1));
  next.addEventListener("click", () => show(activeIndex + 1));
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) dialog.close();
  });
  dialog.addEventListener("keydown", (event) => {
    if (event.key === "ArrowLeft" && gallery.length > 1) show(activeIndex - 1);
    if (event.key === "ArrowRight" && gallery.length > 1) show(activeIndex + 1);
  });
  dialog.addEventListener("close", () => {
    image.removeAttribute("src");
    gallery = [];
  });
})();
