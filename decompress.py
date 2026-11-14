"""Helper script to decompress a zlib-compressed Git object file."""

import argparse
import zlib


def decompress_file(input_file, output_file):
    """Decompresses `input_file` using standard zlib and writes raw bytes to `output_file`."""
    with open(input_file, 'rb') as f:
        compressed_data = f.read()

    decompressed_data = zlib.decompress(compressed_data)

    with open(output_file, 'wb') as f:
        f.write(decompressed_data)


def main():
    """Parses CLI arguments and runs zlib decompression."""
    parser = argparse.ArgumentParser(description='Decompress a zlib-compressed Git object file.')
    parser.add_argument('input_file', help='The input file to decompress')
    parser.add_argument('output_file', help='The output file to write the decompressed data')

    args = parser.parse_args()
    decompress_file(args.input_file, args.output_file)


if __name__ == "__main__":
    main()
