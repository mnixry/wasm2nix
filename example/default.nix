{ }:
let
  rt = import ./generated/yaml_to_json-rt.nix { };
  wasm = import ./generated/yaml_to_json.nix { inherit rt; };

  floorMod = value: divisor: value - divisor * (value / divisor);

  printableAscii = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
  printableStart = 32;
  printableLen = builtins.stringLength printableAscii;

  specialAsciiEntries = [
    {
      name = "\t";
      value = 9;
    }
    {
      name = "\n";
      value = 10;
    }
    {
      name = "\r";
      value = 13;
    }
  ];

  printableAsciiEntries = builtins.genList (i: {
    name = builtins.substring i 1 printableAscii;
    value = printableStart + i;
  }) printableLen;

  stringToCharacters =
    value: builtins.genList (i: builtins.substring i 1 value) (builtins.stringLength value);

  charToByteMap = builtins.listToAttrs (specialAsciiEntries ++ printableAsciiEntries);

  charToByte =
    ch:
    if builtins.hasAttr ch charToByteMap then
      builtins.getAttr ch charToByteMap
    else
      throw "yamlToJson only accepts ASCII input in this Nix wrapper";

  checkAsciiByte =
    byte:
    if byte >= 0 && byte < 128 then
      byte
    else
      throw "yamlToJson only supports ASCII bytes in this Nix wrapper";

  stringToBytes = value: builtins.map (ch: checkAsciiByte (charToByte ch)) (stringToCharacters value);

  hexChars = "0123456789abcdef";

  hexNibble = value: builtins.substring value 1 hexChars;
  hexByte = byte: hexNibble (byte / 16) + hexNibble (floorMod byte 16);

  byteToJsonEscape = byte: "\\u00${hexByte (checkAsciiByte byte)}";

  bytesToString =
    bytes:
    builtins.fromJSON "\"${builtins.concatStringsSep "" (builtins.map byteToJsonEscape bytes)}\"";

  readLastOutput =
    st:
    let
      ptr = builtins.elemAt (wasm.last_output_ptr st).results 0;
      len = builtins.elemAt (wasm.last_output_len st).results 0;
    in
    bytesToString (rt.mem.load_bytes ptr len st);

  yamlToJson =
    input:
    let
      bytes = stringToBytes input;
      instance = wasm.instantiate { };
      allocated = wasm.alloc (builtins.length bytes) instance.st;
      inputPtr = builtins.elemAt allocated.results 0;
      stWithInput = rt.mem.store_bytes bytes inputPtr allocated.st;
      converted = wasm.yaml_to_json inputPtr (builtins.length bytes) stWithInput;
      status = builtins.elemAt converted.results 0;
      output = readLastOutput converted.st;
    in
    if status == 0 then output else throw "yamlToJson failed: ${output}";
in
{
  inherit
    bytesToString
    stringToCharacters
    stringToBytes
    yamlToJson
    ;
}
