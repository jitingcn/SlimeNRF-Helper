#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOCFPlugIn.h>
#include <IOKit/usb/IOUSBLib.h>
#include <stdint.h>

// Read IOKit's cached descriptor without opening or claiming the USB device.
CFDataRef slimenrf_copy_usb_configuration(io_service_t service, uint8_t configuration) {
    IOCFPlugInInterface **plugin = NULL;
    SInt32 score = 0;
    IOReturn result = IOCreatePlugInInterfaceForService(
        service, kIOUSBDeviceUserClientTypeID, kIOCFPlugInInterfaceID, &plugin, &score);
    if (result != kIOReturnSuccess || plugin == NULL) {
        return NULL;
    }

    IOUSBDeviceInterface500 **device = NULL;
    HRESULT query = (*plugin)->QueryInterface(
        plugin, CFUUIDGetUUIDBytes(kIOUSBDeviceInterfaceID500), (LPVOID *)&device);
    (*plugin)->Release(plugin);
    if (query != 0 || device == NULL) {
        return NULL;
    }

    CFDataRef data = NULL;
    UInt8 count = 0;
    if ((*device)->GetNumberOfConfigurations(device, &count) == kIOReturnSuccess) {
        for (UInt8 index = 0; index < count; index++) {
            IOUSBConfigurationDescriptorPtr descriptor = NULL;
            result = (*device)->GetConfigurationDescriptorPtr(device, index, &descriptor);
            if (result != kIOReturnSuccess || descriptor == NULL) {
                continue;
            }
            const uint8_t *bytes = (const uint8_t *)descriptor;
            CFIndex length = bytes[2] | (bytes[3] << 8);
            if (length >= 9 && bytes[5] == configuration) {
                data = CFDataCreate(kCFAllocatorDefault, bytes, length);
                break;
            }
        }
    }
    (*device)->Release(device);
    return data;
}
