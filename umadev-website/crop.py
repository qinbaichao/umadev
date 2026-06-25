from PIL import Image, ImageChops

def crop_inner_window(image_path):
    img = Image.open(image_path).convert("RGB")
    # Let's find the bounding box of the inner window
    # Assuming the outer background has a consistent color at the corners.
    bg = Image.new(img.mode, img.size, img.getpixel((0, 0)))
    diff = ImageChops.difference(img, bg)
    diff = ImageChops.add(diff, diff, 2.0, -100)
    bbox = diff.getbbox()
    if bbox:
        cropped = img.crop(bbox)
        cropped.save(image_path)
        print(f"Cropped {image_path} to {bbox}")
    else:
        print(f"Could not find bounding box for {image_path}")

crop_inner_window("public/assets/1_v2.png")
crop_inner_window("public/assets/2_v2.png")
crop_inner_window("public/assets/3_v2.png")
