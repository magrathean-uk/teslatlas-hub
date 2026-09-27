package main

import (
	"testing"

	"github.com/teslamotors/fleet-telemetry/messages"
	"github.com/teslamotors/fleet-telemetry/messages/tesla"
	"github.com/teslamotors/fleet-telemetry/protos"
	"github.com/teslamotors/fleet-telemetry/telemetry"
	"google.golang.org/protobuf/proto"
)

const (
	testVIN       = "5YJ3E1EA7KF000099"
	testTxid      = "edge-r4-test-v-001"
	testCreatedAt = int64(1900000000000)
)

func TestBuildPayloadIsVehicleNameOnlyWithExplicitTimestamp(t *testing.T) {
	encoded, err := buildPayload(testVIN, testCreatedAt)
	if err != nil {
		t.Fatalf("build payload: %v", err)
	}

	payload := new(protos.Payload)
	if err := proto.Unmarshal(encoded, payload); err != nil {
		t.Fatalf("unmarshal payload: %v", err)
	}
	if got := payload.GetVin(); got != testVIN {
		t.Fatalf("VIN = %q, want %q", got, testVIN)
	}
	if got := payload.GetCreatedAt().AsTime().UnixMilli(); got != testCreatedAt {
		t.Fatalf("created_at_ms = %d, want %d", got, testCreatedAt)
	}
	if got := len(payload.GetData()); got != 1 {
		t.Fatalf("datum count = %d, want one", got)
	}
	datum := payload.GetData()[0]
	if datum.GetKey() != protos.Field_VehicleName {
		t.Fatalf("field = %v, want VehicleName", datum.GetKey())
	}
	if got := datum.GetValue().GetStringValue(); got != vehicleName {
		t.Fatalf("VehicleName = %q, want %q", got, vehicleName)
	}
}

func TestBuildWireEnvelopeUsesOneTopicTransactionAndDerivedMessageID(t *testing.T) {
	encoded, err := buildWireEnvelope(testVIN, testTxid, testCreatedAt)
	if err != nil {
		t.Fatalf("build envelope: %v", err)
	}

	message, err := messages.StreamMessageFromBytes(encoded)
	if err != nil {
		t.Fatalf("parse envelope: %v", err)
	}
	if got := message.Topic(); got != wireTopic {
		t.Fatalf("topic = %q, want %q", got, wireTopic)
	}
	if got := string(message.Txid()); got != testTxid {
		t.Fatalf("txid = %q, want %q", got, testTxid)
	}
	if got := string(message.MessageID()); got != testTxid+messageIDSuffix {
		t.Fatalf("message id = %q, want derived id", got)
	}
	if got := message.CreatedAt; got != uint32(testCreatedAt/1000) {
		t.Fatalf("wire created_at seconds = %d, want %d", got, testCreatedAt/1000)
	}
	if got := string(message.DeviceType); got != "vehicle_device" {
		t.Fatalf("device type = %q, want vehicle_device", got)
	}
	if got := string(message.DeviceID); got != testVIN {
		t.Fatalf("device id = %q, want VIN-derived device id", got)
	}
}

func TestValidateAckRequiresMatchingBinaryVehicleAck(t *testing.T) {
	serializer := telemetry.NewBinarySerializer(nil, nil, nil)
	record := &telemetry.Record{Serializer: serializer, Txid: testTxid, TxType: wireTopic}
	valid := record.Ack()
	if err := validateAck(valid, testTxid); err != nil {
		t.Fatalf("exact pinned receiver ACK rejected: %v", err)
	}

	wrongTransaction := tesla.FlatbuffersStreamAckToBytes(
		[]byte("wrong-txid"),
		[]byte(wireTopic),
		nil,
	)
	if err := validateAck(wrongTransaction, testTxid); err == nil {
		t.Fatal("mismatched ACK transaction was accepted")
	}

	wrongTopic := tesla.FlatbuffersStreamAckToBytes(
		[]byte(testTxid),
		[]byte("D"),
		nil,
	)
	if err := validateAck(wrongTopic, testTxid); err == nil {
		t.Fatal("mismatched ACK topic was accepted")
	}

	unexpectedMessageID := tesla.FlatbuffersStreamAckToBytes(
		[]byte(testTxid),
		[]byte(wireTopic),
		[]byte(testTxid+messageIDSuffix),
	)
	if err := validateAck(unexpectedMessageID, testTxid); err == nil {
		t.Fatal("unexpected nonempty ACK message id was accepted")
	}
}

func TestValidationRejectsMalformedEndpointAndVINWithoutOpeningNetwork(t *testing.T) {
	if err := validateEndpoint("https://127.0.0.1:29999/"); err == nil {
		t.Fatal("non-WebSocket endpoint was accepted")
	}
	if err := validateEndpoint("wss://127.0.0.1:29999/"); err != nil {
		t.Fatalf("valid WebSocket endpoint rejected: %v", err)
	}
	if err := validateVIN("5YJ3E1EA7KF00000I"); err == nil {
		t.Fatal("VIN containing forbidden character was accepted")
	}
	if err := validateVIN(testVIN); err != nil {
		t.Fatalf("valid VIN rejected: %v", err)
	}
}
