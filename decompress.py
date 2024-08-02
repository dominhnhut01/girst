import zlib
import argparse

def decompress_file(input_file, output_file):
    # Read the compressed data
    with open(input_file, 'rb') as f:
        compressed_data = f.read()

    # Decompress the data using raw deflate
    decompressor = zlib.decompressobj(-zlib.MAX_WBITS)  # -MAX_WBITS to indicate raw deflate
    decompressed_data = decompressor.decompress(compressed_data)
    decompressed_data += decompressor.flush()

    # Write the decompressed data to a file
    with open(output_file, 'wb') as f:
        f.write(decompressed_data)

def main():
    parser = argparse.ArgumentParser(description='Decompress a file using raw deflate.')
    parser.add_argument('input_file', help='The input file to decompress')
    parser.add_argument('output_file', help='The output file to write the decompressed data')

    args = parser.parse_args()
    decompress_file(args.input_file, args.output_file)

if __name__ == "__main__":
    main()
